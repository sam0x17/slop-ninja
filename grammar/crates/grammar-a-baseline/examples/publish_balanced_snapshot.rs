//! Publish the frozen fresh non-Test cohort without changing its original rows.
//! Usage: publish_balanced_snapshot REPO COHORT NEW_DATASETS_CHILD
//! Never opens Test prose, full source books, model weights, or provider logs.
//! The older publisher and its historical guards remain independent.
#![recursion_limit = "256"]
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Write,
    path::{Component, Path},
};
const SPAN_SCHEMA: &str = "slop_ninja_realized_span_share_v1";
const DATASET_MANIFEST_SHA: &str =
    "cc5a7ae7de4789c06b935a6066811cd917e16adebec3ab999cdb07279c9e697f";
const REGISTRY_SHA: &str = "dc1eae8acbb64a7d998c0eb1dd20f5474ca5db01135ba387410d0e0225f81fb1";
const LICENSE_INDEX_SHA: &str = "46489865139768e5d3a4669c02475663d63e7d1c9dfe43a258e2d5694587ba5a";
const SLOTS: [&str; 3] = ["known_model_1", "known_model_2", "heldout_CAL_only"];
// These narrowly shared validation patterns intentionally mirror the historical
// publisher, without altering its model pin, historical source or Test guards.
pub(crate) fn sha(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}
pub(crate) fn s<'a>(v: &'a Value, k: &str) -> Result<&'a str> {
    v[k].as_str().with_context(|| format!("missing string {k}"))
}
pub(crate) fn digest(v: &Value, k: &str) -> Result<()> {
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
pub(crate) fn fields(v: &Value, keys: &[&str], omitted: &[&str]) -> Result<Value> {
    let object = v.as_object().context("expected publication object")?;
    ensure!(
        object
            .keys()
            .all(|k| keys.contains(&k.as_str()) || omitted.contains(&k.as_str())),
        "unreviewed publication field"
    );
    Ok(take(v, keys))
}
pub(crate) fn flat(v: &Value, keys: &[&str], omitted: &[&str]) -> Result<Value> {
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

pub(crate) fn public_rights(rights: &Value) -> Result<Value> {
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
pub(crate) fn validate_targets(row: &Value) -> Result<()> {
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

pub(crate) fn public_safety(v: &Value) -> Result<()> {
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

pub(crate) fn compact_sha(v: &Value) -> Result<String> {
    Ok(sha(&serde_json::to_vec(v)?))
}
pub(crate) fn array<'a>(v: &'a Value, key: &str) -> Result<&'a Vec<Value>> {
    v[key]
        .as_array()
        .with_context(|| format!("missing array {key}"))
}
fn ids(v: &Value, key: &str) -> Result<BTreeSet<String>> {
    let mut out = BTreeSet::new();
    for id in array(v, key)? {
        ensure!(
            out.insert(id.as_str().context("ID string")?.to_owned()),
            "duplicate ID"
        );
    }
    Ok(out)
}
fn read_local(root: &Path, relative: &str) -> Result<Vec<u8>> {
    let mut path = root.to_owned();
    for part in Path::new(relative).components() {
        ensure!(matches!(part, Component::Normal(_)), "nonlocal input path");
        path.push(part);
        ensure!(
            !fs::symlink_metadata(&path)?.file_type().is_symlink(),
            "symlink input refused"
        );
    }
    let meta = fs::metadata(&path)?;
    ensure!(
        meta.is_file() && meta.len() <= 64 * 1024 * 1024,
        "input type/size"
    );
    Ok(fs::read(path)?)
}
fn load(root: &Path, relative: &str, expected: Option<&str>) -> Result<Value> {
    let bytes = read_local(root, relative)?;
    if let Some(expected) = expected {
        ensure!(sha(&bytes) == expected, "input digest differs: {relative}");
    }
    Ok(serde_json::from_slice(&bytes)?)
}
fn non_test(split: &str) -> Result<()> {
    ensure!(
        matches!(split, "train" | "development" | "calibration"),
        "Test/unknown partition forbidden"
    );
    Ok(())
}
fn manifest_guard(manifest: &Value, queries: &Value) -> Result<()> {
    ensure!(
        manifest["schema"] == "slop_ninja_a_supervised_dataset_v1"
            && manifest["complete"] == true
            && manifest["test_text_opened"] == false,
        "incomplete or Test dataset manifest"
    );
    ensure!(
        queries["schema"] == "slop_ninja_author_queries_v2"
            && queries["mode"] == "fresh-balanced"
            && queries["test_mode"] == false
            && queries["test_text_opened"] == false,
        "Test/query manifest refused"
    );
    ensure!(
        ids(manifest, "non_test_ids")?.is_disjoint(&ids(manifest, "test_ids")?),
        "reserved ID overlap"
    );
    ensure!(
        ids(queries, "emitted_reference_ids")?.is_disjoint(&ids(queries, "test_ids")?),
        "reserved reference overlap"
    );
    Ok(())
}
fn project_source(row: &Value) -> Result<Value> {
    project_source_scoped(row, false)
}
// The historical publisher always uses false. A successor must separately bind
// a completed Test evaluation before opting into the consumed-Test scope.
pub(crate) fn project_source_scoped(row: &Value, consumed_test: bool) -> Result<Value> {
    let source = &row["source"];
    fields(
        source,
        &[
            "record",
            "source_manifest_sha256",
            "byte_span",
            "extraction",
        ],
        &[],
    )?;
    let r = &source["record"];
    let pg = r["pg_id"].as_u64().context("PG ID")?;
    let split = s(r, "split")?
        .strip_suffix("_reference")
        .unwrap_or(s(r, "split")?);
    if consumed_test {
        ensure!(split == "test", "consumed-Test source scope");
    } else {
        non_test(split)?;
    }
    ensure!(
        row["frozen_split"] == split
            && row["source_group"] == r["source_group"]
            && r["underlying_work_key"] == r["source_group"]
            && row["purpose"] == r["purpose"],
        "source partition/purpose/family differs"
    );
    ensure!(
        r["rights"]["catalog_statement"] == "Public domain in the USA."
            && r["rights"]["jurisdiction"] == "US"
            && r["rights"]["commercial_training"] == true
            && r["rights"]["external_evaluation"] == true
            && row["rights"] == r["rights"]
            && r["rights"]["redistribute_text"] != false,
        "source rights differ/denied"
    );
    ensure!(
        pg > 0
            && r["catalog_url"] == format!("https://www.gutenberg.org/ebooks/{pg}.rdf")
            && s(r, "source_url")?.starts_with("https://www.gutenberg.org/"),
        "source URL differs"
    );
    let original = if row["generation"].is_null() {
        s(row, "text")?
    } else {
        s(&row["share_evidence"], "source_text")?
    };
    for text in [original, s(row, "text")?] {
        ensure!(
            !text.to_lowercase().contains("project gutenberg")
                && !text.contains("*** START OF")
                && !text.contains("*** END OF"),
            "PG-branded text requires separate review"
        );
    }
    let bounds = array(source, "byte_span")?;
    ensure!(bounds.len() == 2, "excerpt span pair");
    let start = bounds[0].as_u64().context("excerpt start")?;
    let end = bounds[1].as_u64().context("excerpt end")?;
    ensure!(
        start < end
            && end <= r["text_bytes"].as_u64().context("source length")?
            && end - start == original.len() as u64,
        "excerpt byte length differs"
    );
    digest(source, "source_manifest_sha256")?;
    digest(r, "text_sha256")?;
    let mut record = flat(
        r,
        &[
            "pg_id",
            "author_key",
            "author",
            "catalog_author",
            "title",
            "split",
            "role",
            "purpose",
            "enrollment",
            "author_partition",
            "preassigned_partition",
            "underlying_work_key",
            "source_group",
            "source_group_policy",
            "text_sha256",
            "text_bytes",
            "source_url",
            "catalog_url",
            "catalog_extent_bytes",
            "catalog_extent_matches_download",
            "evidence",
            "content_processing",
        ],
        &[
            "text_path",
            "text_relative_path",
            "catalog_evidence",
            "raw_download",
            "selection_row",
            "rights",
            "dates",
            "exposure_caveats",
        ],
    )?;
    record["rights"] = public_rights(&r["rights"])?;
    record["dates"] = flat(
        &r["dates"],
        &[
            "pg_ebook_release_date",
            "pg_selected_file_modified",
            "publication_year",
            "first_publication_date",
            "original_composition_date",
            "note",
        ],
        &[],
    )?;
    record["exposure_caveats"] = flat(
        &r["exposure_caveats"],
        &[
            "pretraining_exposure",
            "human_only_production_history",
            "textual_edition_contamination",
            "prior_source_exclusion",
        ],
        &[],
    )?;
    record["catalog_evidence"] = flat(&r["catalog_evidence"], &["sha256", "url"], &["path"])?;
    digest(&record["catalog_evidence"], "sha256")?;
    ensure!(
        record["catalog_evidence"]["sha256"] == r["rights"]["evidence_sha256"]
            && record["catalog_evidence"]["url"] == r["catalog_url"],
        "catalog rights binding"
    );
    let raw = &r["raw_download"];
    record["raw_download"] = flat(
        raw,
        &[
            "binding_sha256",
            "requested_url",
            "effective_url",
            "status",
            "content_type",
            "http_date",
            "http_last_modified",
            "download_started_utc",
            "sha256",
            "bytes",
        ],
        &["path", "receipts"],
    )?;
    record["raw_download"]["receipts"] = array(raw, "receipts")?
        .iter()
        .map(|v| flat(v, &["sha256", "bytes"], &["path"]))
        .collect::<Result<Vec<_>>>()?
        .into();
    ensure!(
        raw["status"] == 200
            && raw["sha256"] == r["text_sha256"]
            && raw["bytes"] == r["text_bytes"]
            && raw["requested_url"] == r["source_url"],
        "download binding differs"
    );
    record["selection_row"] = flat(
        &r["selection_row"],
        &[
            "proposal_order",
            "author_work_order",
            "pg_id",
            "author_key",
            "author",
            "catalog_author",
            "expected_title",
            "underlying_work_key",
            "split",
            "role",
            "enrollment",
            "author_partition",
            "planned_excerpts",
            "language",
            "rights",
            "rights_evidence_url",
            "metadata_sha256",
            "catalog_url",
            "independence_status",
            "catalog_subjects",
        ],
        &["metadata_path"],
    )?;
    Ok(
        json!({"kind":"underlying_us_public_domain_work", "record":record,
        "source_manifest_sha256":source["source_manifest_sha256"],"byte_span":source["byte_span"],"extraction":source["extraction"]}),
    )
}

pub(crate) fn project_config(config: &Value) -> Result<Value> {
    ensure!(
        config["schema"] == "slop_ninja_local_generator_v1"
            && config["rights"]["generation_permitted"] == true
            && config["rights"]["model_weights_redistributed"] == false,
        "generator permission/schema"
    );
    let mut out = flat(
        config,
        &[
            "schema",
            "identity",
            "repo",
            "revision",
            "filename",
            "sha256",
            "size_bytes",
            "runtime_sha256",
            "alias",
            "allowed_splits",
            "original_revision_is_proven_quantization_input",
        ],
        &[
            "model_path",
            "runtime_path",
            "endpoint",
            "runtime_libraries",
            "rights",
        ],
    )?;
    out["runtime_libraries"] = array(config, "runtime_libraries")?
        .iter()
        .map(|v| {
            let mut result = flat(v, &["sha256"], &["path"])?;
            result["filename"] = Path::new(s(v, "path")?)
                .file_name()
                .context("library basename")?
                .to_str()
                .context("UTF8 filename")?
                .into();
            digest(v, "sha256")?;
            Ok(result)
        })
        .collect::<Result<Vec<_>>>()?
        .into();
    out["rights"] = flat(
        &config["rights"],
        &[
            "generation_permitted",
            "model_license",
            "model_weights_redistributed",
        ],
        &["evidence"],
    )?;
    for key in ["sha256", "runtime_sha256"] {
        digest(config, key)?;
    }
    ensure!(
        config["original_revision_is_proven_quantization_input"] == false,
        "unproven conversion revision claim"
    );
    Ok(out)
}

struct Generator {
    slot: String,
    config: Value,
    config_sha: String,
    registry: Value,
    binding: Value,
    report: Value,
}
fn project_generation(
    row: &Value,
    generator: &Generator,
    cohort: &Path,
    evidence: &BTreeMap<String, String>,
) -> Result<Value> {
    let g = &row["generation"];
    let b = &g["binding"];
    ensure!(
        g["generator"] == generator.config["identity"]
            && row["ai_identity"] == g["generator"]
            && b == &generator.binding
            && b["generator_config"] == generator.config
            && b["generator_config_sha256"] == generator.config_sha
            && b["test_opened"] == false,
        "generator/binding/config identity differs"
    );
    ensure!(
        generator.config["allowed_splits"]
            .as_array()
            .context("allowed splits")?
            .contains(&row["frozen_split"]),
        "generator split denied"
    );
    let mut out = flat(
        g,
        &[
            "generator",
            "position",
            "response_sha256",
            "mode",
            "attempt",
            "response_format_policy",
        ],
        &["binding", "request", "response_path"],
    )?;
    let mut binding = flat(
        b,
        &[
            "schema",
            "input_sha256",
            "generator",
            "generator_config_sha256",
            "producer_sha256",
            "share_word_policy",
            "requested_span_percentages",
            "span_policy",
            "seed",
            "temperature",
            "top_p",
            "max_tokens",
            "thinking",
            "input_schema",
            "failures",
            "test_opened",
        ],
        &["generator_config", "endpoint"],
    )?;
    ensure!(
        b["schema"] == "slop_ninja_controlled_generation_v2" && b["thinking"] == false,
        "generation contract"
    );
    binding["generator_config"] = project_config(&generator.config)?;
    binding["original_binding_sha256"] = compact_sha(b)?.into();
    out["binding"] = binding;
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
    ensure!(
        request["model"] == generator.config["alias"],
        "request model alias differs"
    );
    for key in ["temperature", "top_p", "seed", "max_tokens"] {
        ensure!(request[key] == b[key], "sampler differs");
    }
    out["request"]["messages"] = array(request, "messages")?
        .iter()
        .map(|message| {
            ensure!(message["role"] == "user", "unreviewed prompt role");
            flat(message, &["role", "content"], &[])
        })
        .collect::<Result<Vec<_>>>()?
        .into();
    ensure!(!array(request, "messages")?.is_empty(), "missing prompts");
    out["request"]["chat_template_kwargs"] =
        flat(&request["chat_template_kwargs"], &["enable_thinking"], &[])?;
    out["request_sha256"] = compact_sha(request)?.into();
    out["prompt_content_sha256"] = array(request, "messages")?
        .iter()
        .map(|message| Ok(json!(sha(s(message, "content")?.as_bytes()))))
        .collect::<Result<Vec<_>>>()?
        .into();
    digest(g, "response_sha256")?;
    let id = s(row, "id")?;
    ensure!(
        id.starts_with("local-") && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-'),
        "unsafe generated ID"
    );
    let relative = format!("generation/{}/{id}/context.json", generator.slot);
    let expected = evidence
        .get(&relative)
        .context("unbound context metadata")?;
    let context = load(cohort, &relative, Some(expected))?;
    out["context"] = flat(
        &context,
        &[
            "request_sha256",
            "rendered_prompt_sha256",
            "prompt_tokens",
            "max_output_tokens",
            "context_capacity",
            "fits",
        ],
        &[],
    )?;
    ensure!(
        context["request_sha256"] == out["request_sha256"] && context["fits"] == true,
        "context/request binding"
    );
    digest(&context, "rendered_prompt_sha256")?;
    out["context_receipt_sha256"] = expected.clone().into();
    // These are hashes of archived attempts, never the attempts themselves.
    let prefix = format!("generation/{}/{id}/", generator.slot);
    out["attempt_history"] = evidence
        .iter()
        .filter_map(|(path, hash)| {
            let name = path.strip_prefix(&prefix)?;
            (name.starts_with("attempt-") && !name.contains('/'))
                .then(|| json!({"receipt":name,"sha256":hash}))
        })
        .collect::<Vec<_>>()
        .into();
    let response_name = format!(
        "{prefix}attempt-{}.json",
        g["attempt"].as_u64().context("attempt")?
    );
    ensure!(
        evidence
            .get(&response_name)
            .is_some_and(|h| g["response_sha256"] == *h),
        "response evidence digest differs"
    );
    out["quantizer_and_original_license"] = generator.registry.clone();
    out["raw_invocations_retained_privately"] = true.into();
    Ok(out)
}

fn validate_population(rows: &BTreeMap<String, Value>) -> Result<()> {
    validate_population_scoped(rows, false)
}
pub(crate) fn validate_population_scoped(
    rows: &BTreeMap<String, Value>,
    include_consumed_test: bool,
) -> Result<()> {
    let mut groups = BTreeMap::new();
    let mut hashes = BTreeMap::new();
    for row in rows.values() {
        let split = s(row, "frozen_split")?;
        if split != "test" || !include_consumed_test {
            non_test(split)?;
        }
        let group = s(row, "source_group")?;
        let purpose = s(row, "purpose")?;
        ensure!(matches!(purpose, "query" | "reference"), "unknown purpose");
        if let Some(previous) = groups.insert(group, (split, purpose)) {
            ensure!(
                previous == (split, purpose),
                "work crosses partition/reference boundary"
            );
        }
        if let Some(previous) = hashes.insert(s(row, "text_sha256")?, (split, purpose)) {
            ensure!(
                previous == (split, purpose),
                "exact text crosses partition/reference boundary"
            );
        }
        if !row["generation"].is_null() {
            let parent = rows
                .get(s(row, "parent_id")?)
                .context("generated parent missing")?;
            ensure!(
                parent["origin"] == "human_only"
                    && parent["generation"].is_null()
                    && parent["purpose"] == "query"
                    && row["purpose"] == "query",
                "reference/nonhuman generation parent"
            );
            for key in ["source", "rights", "frozen_split", "source_group"] {
                ensure!(
                    row[key] == parent[key],
                    "descendant source/partition changed"
                );
            }
            ensure!(
                row["share_evidence"]["source_text"] == parent["text"]
                    && row["share_evidence"]["source_sha256"] == parent["text_sha256"],
                "descendant parent bytes differ"
            );
        } else {
            ensure!(
                row["origin"] == "human_only"
                    && row["parent_id"].is_null()
                    && row["human_identity"] == row["source"]["record"]["author_key"],
                "unrecorded human history"
            );
        }
    }
    Ok(())
}

fn project(
    row: &Value,
    cohort: &Path,
    generators: &[Generator],
    evidence: &BTreeMap<String, String>,
    line_hash: &str,
) -> Result<Value> {
    ensure!(
        row["schema"] == "slop_ninja_a_supervised_record_v1"
            && row["text_sha256"] == sha(s(row, "text")?.as_bytes()),
        "record schema/text digest"
    );
    non_test(s(row, "frozen_split")?)?;
    validate_targets(row)?;
    ensure!(
        row["documented_human_only"] == false && row["exposed_to_grammar"].is_boolean(),
        "proxy/exposure contract"
    );
    let mut out = flat(
        row,
        &[
            "schema",
            "id",
            "text",
            "text_sha256",
            "source_group",
            "frozen_split",
            "purpose",
            "origin",
            "evidence",
            "documented_human_only",
            "human_identity",
            "human_role_present",
            "ai_identity",
            "ai_role_present",
            "share_target",
            "exposed_to_grammar",
            "grammar_exposure",
            "parent_id",
        ],
        &["share_evidence", "source", "rights", "generation"],
    )?;
    out["share_evidence"] = row["share_evidence"].clone();
    out["source"] = project_source(row)?;
    out["rights"] = public_rights(&row["rights"])?;
    if !row["generation"].is_null() {
        let generator = generators
            .iter()
            .find(|g| row["ai_identity"] == g.config["identity"])
            .context("unreviewed generator")?;
        out["generation"] = project_generation(row, generator, cohort, evidence)?;
    }
    out["publication"] = json!({"original_line_sha256":line_hash,
        "line_hash_basis":"exact original JSONL line bytes including terminating newline",
        "original_row_sha256":compact_sha(row)?,"row_hash_basis":"serde_json compact Value encoding",
        "projection":"allow-listed metadata; exact text, targets, spans and history unchanged",
        "public_exposed_population":true,"future_hidden_test":false});
    public_safety(&out)?;
    Ok(out)
}

fn append_rows(
    bytes: &[u8],
    expected: &BTreeSet<String>,
    reference_file: bool,
    rows: &mut BTreeMap<String, Value>,
    line_hashes: &mut BTreeMap<String, String>,
) -> Result<()> {
    let mut seen = BTreeSet::new();
    for line in bytes.split_inclusive(|b| *b == b'\n') {
        ensure!(line.last() == Some(&b'\n'), "JSONL final newline required");
        let row: Value = serde_json::from_slice(line)?;
        let id = s(&row, "id")?.to_owned();
        ensure!(
            expected.contains(&id) && seen.insert(id.clone()),
            "unexpected/duplicate record ID"
        );
        non_test(s(&row, "frozen_split")?)?;
        if reference_file {
            ensure!(
                row["purpose"] == "reference",
                "nonreference in reference file"
            );
        }
        if let Some(prior) = rows.get(&id) {
            ensure!(
                reference_file
                    && row["purpose"] == "reference"
                    && row["frozen_split"] == "train"
                    && prior == &row,
                "duplicate row mismatch"
            );
            ensure!(
                line_hashes[&id] == sha(line),
                "duplicated reference line bytes differ"
            );
        } else {
            line_hashes.insert(id.clone(), sha(line));
            rows.insert(id, row);
        }
    }
    ensure!(&seen == expected, "manifest ID set differs");
    Ok(())
}

pub(crate) fn registry_model(v: &Value) -> Result<Value> {
    flat(
        v,
        &[
            "role",
            "original_model_repo",
            "original_metadata_revision",
            "original_revision_is_proven_quantization_input",
            "original_license",
            "quantizer_card_license",
            "quantized_repo",
            "revision",
            "filename",
            "size_bytes",
            "sha256",
            "lfs_oid",
            "git_pointer_blob_sha1",
            "download_url",
            "quantization",
            "architecture",
            "advertised_context_tokens",
            "original_gated",
            "quantization_gated",
        ],
        &["download_needed", "observed_weight_hash_locally_recomputed"],
    )
}

fn run(repo: &Path, cohort: &Path, out: &Path) -> Result<Value> {
    let repo = repo.canonicalize()?;
    let cohort = cohort.canonicalize()?;
    ensure!(
        !out.exists()
            && out.parent().context("output parent")?.canonicalize()? == repo.join("datasets"),
        "new datasets child required"
    );
    let manifest = load(
        &cohort,
        "dataset-v1/manifest.json",
        Some(DATASET_MANIFEST_SHA),
    )?;
    let queries = load(
        &cohort,
        "queries-v1/manifest.json",
        Some(s(&manifest, "reserved_manifest_sha256")?),
    )?;
    manifest_guard(&manifest, &queries)?; // Before opening either prose file.
    let sources = load(
        &cohort,
        "sources-v1/manifest.json",
        Some(s(&manifest, "source_manifest_sha256")?),
    )?;
    let registry = load(&cohort, "generator-identities.json", Some(REGISTRY_SHA))?;
    let index = read_local(&cohort, "generator-evidence.sha256")?;
    ensure!(
        sha(&index) == LICENSE_INDEX_SHA,
        "license evidence index changed"
    );
    let index = std::str::from_utf8(&index)?
        .lines()
        .map(|line| {
            let (hash, name) = line.split_once("  ").context("license index line")?;
            Ok((name.to_owned(), hash.to_owned()))
        })
        .collect::<Result<BTreeMap<_, _>>>()?;
    // Only lookup fixed metadata suffixes under COHORT. Never follow arbitrary
    // manifest locators into source books, weights, Test or invocation logs.
    let mut evidence = BTreeMap::new();
    for entry in array(&manifest, "sources")? {
        let relative = Path::new(s(entry, "path")?)
            .strip_prefix(&cohort)
            .context("manifest source outside cohort")?
            .to_str()
            .context("input path UTF8")?;
        digest(entry, "sha256")?;
        if let Some(old) = evidence.insert(relative.into(), s(entry, "sha256")?.to_owned()) {
            ensure!(old == entry["sha256"], "conflicting evidence digest");
        }
    }
    let mut generators = Vec::new();
    for slot in SLOTS {
        let report = array(&manifest, "generation_reports")?
            .iter()
            .find(|r| r["slot"] == slot)
            .context("generator report missing")?;
        ensure!(report["complete"] == true, "incomplete generation report");
        let config_sha = s(report, "config_sha256")?.to_owned();
        let config = load(&cohort, &format!("configs/{slot}.json"), Some(&config_sha))?;
        let registry = registry_model(
            array(&registry, "generators")?
                .iter()
                .find(|g| g["role"] == slot)
                .context("missing model registry identity")?,
        )?;
        for (a, b) in [
            ("repo", "quantized_repo"),
            ("revision", "revision"),
            ("filename", "filename"),
            ("sha256", "sha256"),
            ("size_bytes", "size_bytes"),
        ] {
            ensure!(config[a] == registry[b], "quantizer identity differs");
        }
        let relative = format!("generation/{slot}/binding.json");
        let binding = load(
            &cohort,
            &relative,
            Some(evidence.get(&relative).context("binding digest")?),
        )?;
        ensure!(
            compact_sha(&binding)? == report["binding_sha256"] && binding["test_opened"] == false,
            "report binding differs"
        );
        generators.push(Generator {
            slot: slot.into(),
            config,
            config_sha,
            registry,
            binding,
            report: report.clone(),
        });
    }
    let input = read_local(&cohort, "dataset-v1/records.jsonl")?;
    ensure!(
        sha(&input) == manifest["records_sha256"],
        "dataset bytes differ"
    );
    let refs = read_local(&cohort, "queries-v1/references.jsonl")?;
    ensure!(
        sha(&refs) == queries["references_sha256"],
        "reference bytes differ"
    );
    let mut rows = BTreeMap::new();
    let mut line_hashes = BTreeMap::new();
    append_rows(
        &input,
        &ids(&manifest, "non_test_ids")?,
        false,
        &mut rows,
        &mut line_hashes,
    )?;
    ensure!(
        rows.len() as u64 == manifest["records"].as_u64().context("record count")?,
        "dataset count differs"
    );
    append_rows(
        &refs,
        &ids(&queries, "emitted_reference_ids")?,
        true,
        &mut rows,
        &mut line_hashes,
    )?;
    validate_population(&rows)?;
    let source_by_group = array(&sources, "sources")?
        .iter()
        .map(|v| Ok((s(v, "source_group")?, v)))
        .collect::<Result<BTreeMap<_, _>>>()?;
    let mut files = BTreeMap::<String, Vec<u8>>::new();
    let mut counts = BTreeMap::<String, usize>::new();
    let mut origins = BTreeMap::<String, usize>::new();
    let mut families = BTreeMap::new();
    let mut fractional = 0;
    for (id, row) in &rows {
        let source = source_by_group
            .get(s(row, "source_group")?)
            .context("unlisted source group")?;
        ensure!(
            row["source"]["record"] == **source
                && row["source"]["source_manifest_sha256"] == manifest["source_manifest_sha256"],
            "acquisition provenance differs"
        );
        let projected = project(row, &cohort, &generators, &evidence, &line_hashes[id])?;
        let partition = if row["purpose"] == "reference" {
            "references"
        } else {
            s(row, "frozen_split")?
        };
        let bytes = files.entry(format!("{partition}.jsonl")).or_default();
        serde_json::to_writer(&mut *bytes, &projected)?;
        bytes.push(b'\n');
        *counts.entry(partition.into()).or_default() += 1;
        *origins.entry(s(row, "origin")?.into()).or_default() += 1;
        families.insert(
            s(row, "source_group")?.to_owned(),
            json!({"split":row["frozen_split"],"purpose":row["purpose"]}),
        );
        fractional += usize::from(
            row["share_target"]
                .as_f64()
                .is_some_and(|x| x > 0.0 && x < 1.0),
        );
    }
    let mut failures = Vec::new();
    for generator in &generators {
        for failure in array(&generator.report, "failures")? {
            let mut value = flat(
                failure,
                &[
                    "id",
                    "mode",
                    "parent_id",
                    "reason",
                    "requested_span_percent",
                ],
                &[],
            )?;
            ensure!(
                !rows.contains_key(s(failure, "id")?)
                    && rows.contains_key(s(failure, "parent_id")?),
                "failure population differs"
            );
            value["generator"] = generator.config["identity"].clone();
            value["generator_config_sha256"] = generator.config_sha.clone().into();
            value["binding_sha256"] = generator.report["binding_sha256"].clone();
            value["frozen_split"] = rows[s(failure, "parent_id")?]["frozen_split"].clone();
            let prefix = format!("generation/{}/{}/", generator.slot, s(failure, "id")?);
            value["retained_evidence_hashes"] = evidence
                .iter()
                .filter_map(|(path, hash)| {
                    path.strip_prefix(&prefix)
                        .map(|name| json!({"receipt":name,"sha256":hash}))
                })
                .collect::<Vec<_>>()
                .into();
            failures.push(value);
        }
    }
    ensure!(
        manifest["excluded"] == json!([]) && manifest["extraction_failures"] == json!([]),
        "new exclusion kind needs explicit projection"
    );
    files.insert(
        "failures.json".into(),
        serde_json::to_vec_pretty(
            &json!({"generation":failures,"assembly_excluded":[],"extraction_failures":[]}),
        )?,
    );
    let license_files = [
        "qwen-base.LICENSE",
        "phi-base.LICENSE",
        "gemma-terms.html",
        "gemma-prohibited-use.html",
    ];
    let mut license_index = Vec::new();
    for name in license_files {
        let relative = format!("generator-evidence/{name}");
        let bytes = read_local(&cohort, &relative)?;
        let hash = sha(&bytes);
        ensure!(
            index.get(&relative) == Some(&hash),
            "license evidence changed"
        );
        let text = std::str::from_utf8(&bytes)?;
        ensure!(
            !text.contains("~/")
                && !text.contains("data/grammar-private/")
                && !text.contains("PANGRAM_API_KEY"),
            "private license locator"
        );
        files.insert(format!("licenses/{name}"), bytes);
        license_index.push(json!({"file":format!("licenses/{name}"),"sha256":hash}));
    }
    let runtime = flat(
        &registry["runtime"],
        &["name", "tag", "commit", "variant"],
        &["directory", "runtime_smoke_test_performed"],
    )?;
    files.insert("generators.json".into(), serde_json::to_vec_pretty(&json!({"schema":"slop_ninja_published_generator_registry_v1",
        "generators":generators.iter().map(|g|json!({"model":g.registry,"configuration":project_config(&g.config).unwrap(),"original_config_sha256":g.config_sha,
            "producer_sha256":g.binding["producer_sha256"],"original_binding_sha256":g.report["binding_sha256"]})).collect::<Vec<_>>(),
        "runtime":runtime,"license_evidence":license_index,"original_registry_sha256":REGISTRY_SHA,"model_weights_included":false}))?);
    files.insert("NOTICE.md".into(), NOTICE.as_bytes().to_vec());
    let readme = format!(
        "# Slop Ninja fresh balanced non-Test cohort\n\nThis authorized public projection contains {} records from {} source works: {} Train queries, {} Development queries, {} Calibration queries, and {} independent references. All Test text remains unopened and absent. The references retain their actual `frozen_split`; never use references for fitting, normalization, vocabulary construction or query scoring. Keep whole works and descendants in their frozen partitions.\n\nThe historical human labels are public-domain author proxies, not authenticated human-only workflows. Grammar and pretraining exposure remain conservative and unknown. Publication makes these records exposed research data, not a future hidden benchmark. Publication does not change the original fitting inputs or hashes.\n\nExact text, SHA256, source author/title/URLs, original composition-date uncertainty, acquisition hashes, excerpt bounds, generation prompts, sampler settings, original/quantizer identities, runtime hashes, response and attempt hashes, and exact replacement-span share evidence survive the field-allowlisted projection. A complete rewrite remains mixed contribution history even at unit replacement share. Requested edit percentages are not realized share targets. Raw provider logs, full source books, weights, filesystem locators and Test prose are absent.\n\nThree failed Phi generation tasks remain in `failures.json`; they have no substitute text or fabricated labels. The 12 Train references present in both input files appear once. `generators.json`, `NOTICE.md` and `licenses/` distinguish original model licenses from quantization evidence and source/output rights.\n\nAuthor excerpts rely on recorded **US** public-domain determinations. New generated text and curation are offered under [CC BY-SA 4.0](https://creativecommons.org/licenses/by-sa/4.0/) to the extent rights exist, subject to underlying rights and generator terms. The code license does not replace these text terms. Preserve provenance and notices; see [the model ShareAlike policy](../../baseline/detector/SHARE_ALIKE_POLICY.md).\n\nThe separate Rust `publish_balanced_snapshot` example pins the exact input manifest, checks the non-Test roster, rights, source/group bindings, exact text and spans, parentage, references, licenses and an explicit field allowlist. It never opens source books, model weights or raw provider attempts. Reproduce with `publish_balanced_snapshot REPO COHORT NEW_DATASETS_CHILD`; validate this copy with `sha256sum -c SHA256SUMS`. The original historical publisher is unchanged. Pangram observations are absent and never become training labels.\n",
        rows.len(),
        families.len(),
        counts["train"],
        counts["development"],
        counts["calibration"],
        counts["references"]
    );
    files.insert("README.md".into(), readme.into_bytes());
    let public_manifest = json!({"schema":"slop_ninja_a_balanced_public_snapshot_v1","records":rows.len(),"partitions":counts,"origins":origins,
        "source_families":families.len(),"source_work_partitions":families,"fractional_share_records":fractional,
        "test_text_opened":false,"test_records_published":0,"clean_holdout":false,"future_hidden_test":false,
        "original_dataset_sha256":manifest["records_sha256"],"original_dataset_manifest_sha256":DATASET_MANIFEST_SHA,
        "original_source_manifest_sha256":manifest["source_manifest_sha256"],"original_query_manifest_sha256":manifest["reserved_manifest_sha256"],
        "original_references_sha256":queries["references_sha256"],"frozen_selection_sha256":manifest["frozen_selection_sha256"],
        "original_records_immutable":true,"lineage_basis":"exact original line-byte and compact row hashes retained per record",
        "requested_generation_records":manifest["requested_generation_records"],"accepted_generation_records":origins.get("model_only").copied().unwrap_or(0)+origins.get("mixed").copied().unwrap_or(0),
        "generation_failures":failures.len(),"reference_policy":"separate references retain actual partitions; never fit parameters or normalization on references",
        "rights":"US public-domain source determinations; generated text and curation CC-BY-SA-4.0 to extent rights exist; underlying rights and generator terms retained",
        "publisher_sha256":sha(include_bytes!("publish_balanced_snapshot.rs")),"files":files.iter().map(|(name,bytes)|json!({"path":name,"bytes":bytes.len(),"sha256":sha(bytes)})).collect::<Vec<_>>()});
    public_safety(&public_manifest)?;
    for name in ["generators.json", "failures.json"] {
        public_safety(&serde_json::from_slice(&files[name])?)?;
    }
    files.insert(
        "manifest.json".into(),
        serde_json::to_vec_pretty(&public_manifest)?,
    );
    let stage = tempfile::Builder::new()
        .prefix(".a-balanced-publish-")
        .tempdir_in(repo.join("datasets"))?;
    fs::create_dir(stage.path().join("licenses"))?;
    let mut sums = String::new();
    for (name, bytes) in &files {
        put(&stage.path().join(name), bytes)?;
        sums.push_str(&format!("{}  {name}\n", sha(bytes)));
    }
    put(&stage.path().join("SHA256SUMS"), sums.as_bytes())?;
    fs::File::open(stage.path().join("licenses"))?.sync_all()?;
    fs::File::open(stage.path())?.sync_all()?;
    ensure!(!out.exists(), "output appeared during publication");
    fs::rename(stage.path(), out)?;
    fs::File::open(repo.join("datasets"))?.sync_all()?;
    Ok(
        json!({"records":rows.len(),"partitions":counts,"families":families.len(),"fractional_share_records":fractional,"generation_failures":failures.len(),"test_opened":false}),
    )
}

const NOTICE: &str = "# Source and generator notices\n\nSource excerpts retain their per-work Project Gutenberg catalog evidence for public-domain status in the USA. This is not a worldwide rights determination or an authenticated human-only production history. PG branding is not included in the excerpts. Derivatives preserve the source attribution, dates, exact extraction and replacement spans, and changes recorded in each row.\n\nQwen3.6-35B-A3B: original model Apache-2.0, with the retained original license in `licenses/qwen-base.LICENSE`. Unsloth's UD-IQ4_XS quantizer card separately declares Apache-2.0.\n\nPhi-4-mini-instruct: original model MIT, with the retained Microsoft notice in `licenses/phi-base.LICENSE`. Bartowski's quantizer card states no independent license.\n\nGemma-3-4b-it: custom Gemma Terms of Use, including the Prohibited Use Policy. Complete retained public legal-page snapshots accompany this dataset in `licenses/gemma-terms.html` and `licenses/gemma-prohibited-use.html`; original URLs are https://ai.google.dev/gemma/terms and https://ai.google.dev/gemma/prohibited_use_policy. Gemma is provided under and subject to the Gemma Terms of Use found at ai.google.dev/gemma/terms. This dataset distributes generated text, not Gemma weights. Bartowski's quantizer card states no independent license.\n\nOriginal model revisions in `generators.json` pin inspected metadata/license evidence. They are not proven conversion inputs. Actual generation identities are pinned separately by quantizer repository revision, filename and weight SHA256. Runtime build/library digests and producer hashes remain; no model weights or runtime binaries are distributed here.\n\nNew generated text and curation: CC BY-SA 4.0, to the extent rights exist and subject to underlying rights and generator terms. Attribution: Slop Ninja and the recorded source authors; preserve each row's change history and source attribution.\n";

fn main() -> Result<()> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    ensure!(
        args.len() == 3,
        "publish_balanced_snapshot REPO COHORT NEW_DATASETS_CHILD"
    );
    println!(
        "{}",
        run(
            Path::new(&args[0]),
            Path::new(&args[1]),
            Path::new(&args[2])
        )?
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn human() -> Value {
        let text = "Café waits. A synthetic sentence follows.";
        let rights = json!({"catalog_statement":"Public domain in the USA.","jurisdiction":"US",
            "commercial_training":true,"external_evaluation":true,"evidence_sha256":"b".repeat(64)});
        json!({"schema":"slop_ninja_a_supervised_record_v1","id":"synthetic-human","text":text,
            "text_sha256":sha(text.as_bytes()),"source_group":"work","frozen_split":"train","purpose":"query",
            "origin":"human_only","evidence":"historical_public_domain_author_proxy","documented_human_only":false,
            "human_identity":"writer","human_role_present":true,"ai_identity":null,"ai_role_present":false,
            "share_target":0.0,"share_evidence":{"basis":"historical_human_proxy_endpoint","not_documented_contemporary_workflow":true},
            "exposed_to_grammar":true,"grammar_exposure":"unknown","parent_id":null,"rights":rights,
            "source":{"source_manifest_sha256":"a".repeat(64),"byte_span":[0,text.len()],"extraction":"synthetic",
                "record":{"pg_id":42,"author_key":"writer","source_group":"work","underlying_work_key":"work",
                    "split":"train","purpose":"query","rights":rights,"text_path":"/home/private/source.txt",
                    "text_relative_path":"opaque/source.txt","text_sha256":sha(text.as_bytes()),"text_bytes":text.len(),
                    "source_url":"https://www.gutenberg.org/ebooks/42.txt.utf-8","catalog_url":"https://www.gutenberg.org/ebooks/42.rdf",
                    "dates":{"original_composition_date":null},"exposure_caveats":{"pretraining_exposure":"unknown"},
                    "catalog_evidence":{"path":"/home/private/42.rdf","sha256":"b".repeat(64),"url":"https://www.gutenberg.org/ebooks/42.rdf"},
                    "raw_download":{"status":200,"sha256":sha(text.as_bytes()),"bytes":text.len(),"requested_url":"https://www.gutenberg.org/ebooks/42.txt.utf-8",
                        "path":"private/body","receipts":[{"path":"private/receipt","sha256":"c".repeat(64),"bytes":100}]},
                    "selection_row":{"metadata_path":"private/42.rdf","pg_id":"42"}}}})
    }
    #[test]
    fn human_projection_preserves_bytes_and_rights_without_locators() {
        let row = human();
        let original = row.clone();
        let out = project(
            &row,
            Path::new("/unused"),
            &[],
            &BTreeMap::new(),
            &"d".repeat(64),
        )
        .unwrap();
        for key in [
            "text",
            "text_sha256",
            "share_evidence",
            "rights",
            "frozen_split",
            "grammar_exposure",
        ] {
            assert_eq!(out[key], row[key]);
        }
        assert_eq!(row, original);
        assert!(out["source"]["record"].get("text_path").is_none());
        assert!(
            out["source"]["record"]["catalog_evidence"]
                .get("path")
                .is_none()
        );
        public_safety(&out).unwrap();
    }
    #[test]
    fn unknown_fields_rights_denials_and_credential_strings_fail_closed() {
        let mut row = human();
        row["new_unreviewed_metadata"] = json!({"anything":"value"});
        assert!(project(&row, Path::new("/unused"), &[], &BTreeMap::new(), "hash").is_err());
        let mut row = human();
        row["rights"]["redistribute_text"] = false.into();
        row["source"]["record"]["rights"]["redistribute_text"] = false.into();
        assert!(project_source(&row).is_err());
        for secret in [
            "/home/private/file",
            "data/grammar-private/x",
            "token=synthetic",
            "https://name:secret@example.com/",
        ] {
            assert!(public_safety(&json!({"prompt":secret})).is_err());
        }
    }
    #[test]
    fn mixed_full_rewrite_keeps_history_and_validates_exact_unicode_spans() {
        let mut row = human();
        let source = row["text"].as_str().unwrap().to_owned();
        let replacement = "Élan changes every word.";
        row["origin"] = "mixed".into();
        row["ai_role_present"] = true.into();
        row["ai_identity"] = "synthetic-model".into();
        row["text"] = replacement.into();
        row["text_sha256"] = sha(replacement.as_bytes()).into();
        row["share_target"] = 1.0.into();
        row["generation"] = json!({"mode":"controlled_rewrite"});
        row["share_evidence"] = json!({"schema":SPAN_SCHEMA,"source_text":source,"source_sha256":sha(source.as_bytes()),
            "source_byte_span":[0,source.len()],"replacement":replacement,"final_byte_span":[0,replacement.len()],"final_sha256":row["text_sha256"],
            "replacement_words":4,"final_words":4,"word_policy":"unicode_split_whitespace_v1","realized_share":1.0,
            "requested_percent_not_target":100,"full_rewrite_is_mixed_history":true});
        validate_targets(&row).unwrap();
        row["share_evidence"]["final_byte_span"] = json!([1, replacement.len()]);
        assert!(validate_targets(&row).is_err());
        row["share_evidence"]["final_byte_span"] = json!([0, replacement.len()]);
        row["share_target"] = 0.6.into();
        assert!(validate_targets(&row).is_err());
    }
    #[test]
    fn references_deduplicate_only_exact_train_copies() {
        let mut row = human();
        row["purpose"] = "reference".into();
        let input = format!("{row}\n").into_bytes();
        let expected = BTreeSet::from(["synthetic-human".to_owned()]);
        let mut rows = BTreeMap::new();
        let mut hashes = BTreeMap::new();
        append_rows(&input, &expected, false, &mut rows, &mut hashes).unwrap();
        append_rows(&input, &expected, true, &mut rows, &mut hashes).unwrap();
        assert_eq!(rows.len(), 1);
        row["frozen_split"] = "calibration".into();
        assert!(
            append_rows(
                format!("{row}\n").as_bytes(),
                &expected,
                true,
                &mut rows,
                &mut hashes
            )
            .is_err()
        );
    }
    #[test]
    fn cross_partition_reference_and_parent_corruption_are_rejected() {
        let first = human();
        let mut second = first.clone();
        second["id"] = "second".into();
        second["frozen_split"] = "calibration".into();
        let mut rows =
            BTreeMap::from([("synthetic-human".into(), first), ("second".into(), second)]);
        assert!(validate_population(&rows).is_err());
        rows.get_mut("second").unwrap()["frozen_split"] = "train".into();
        rows.get_mut("second").unwrap()["purpose"] = "reference".into();
        assert!(validate_population(&rows).is_err());
        rows.get_mut("second").unwrap()["purpose"] = "query".into();
        rows.get_mut("second").unwrap()["generation"] = json!({"generator":"synthetic"});
        rows.get_mut("second").unwrap()["parent_id"] = "missing".into();
        assert!(validate_population(&rows).is_err());
    }
    #[test]
    fn test_manifests_and_rows_fail_before_projection() {
        let mut manifest = json!({"schema":"slop_ninja_a_supervised_dataset_v1","complete":true,"test_text_opened":false,
            "non_test_ids":["query"],"test_ids":["reserved"]});
        let queries = json!({"schema":"slop_ninja_author_queries_v2","mode":"fresh-balanced","test_mode":false,
            "test_text_opened":false,"emitted_reference_ids":["reference"],"test_ids":["reserved"]});
        manifest_guard(&manifest, &queries).unwrap();
        manifest["test_text_opened"] = true.into();
        assert!(manifest_guard(&manifest, &queries).is_err());
        for split in ["test", "test_reference"] {
            assert!(non_test(split).is_err());
        }
        let mut row = human();
        row["frozen_split"] = "test".into();
        assert!(
            append_rows(
                format!("{row}\n").as_bytes(),
                &BTreeSet::from(["synthetic-human".into()]),
                false,
                &mut BTreeMap::new(),
                &mut BTreeMap::new()
            )
            .is_err()
        );
    }
    #[cfg(unix)]
    #[test]
    fn symlink_and_parent_traversal_inputs_are_rejected() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("file"), b"{}").unwrap();
        std::os::unix::fs::symlink(root.path().join("file"), root.path().join("link")).unwrap();
        assert!(read_local(root.path(), "link").is_err());
        assert!(read_local(root.path(), "../file").is_err());
    }
}
