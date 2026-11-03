//! Publish the qualified Train-only augmentation as a new immutable snapshot.
//! publish_train_augmentation REPO TRAIN_SCALE NEW_DATASETS_CHILD
//! Pins this release; never opens Test, model weights or raw invocation bodies.
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
#[allow(dead_code)]
#[path = "prepare_supervised_forest/supervision.rs"]
mod supervision;
const PARTS: [&str; 3] = ["train", "development", "calibration"];
fn str_at<'a>(v: &'a Value, key: &str) -> Result<&'a str> {
    let value = s(v, key)?;
    ensure!(!value.is_empty(), "empty {key}");
    Ok(value)
}
fn digest_at<'a>(v: &'a Value, key: &str) -> Result<&'a str> {
    digest(v, key)?;
    str_at(v, key)
}
fn counter(v: &Value, key: &str) -> Result<u64> {
    v[key].as_u64().with_context(|| format!("counter {key}"))
}
fn number(v: &Value) -> Result<f64> {
    let n = v.as_f64().context("number")?;
    ensure!(n.is_finite() && n >= 0., "invalid number");
    Ok(n)
}
const SPAN_SCHEMA: &str = "slop_ninja_realized_span_share_v1";
const MANIFEST_SHA: &str = "b4d556badaf9995827ed9b16429df8178fe7a00e3960df719e0ea7b248e8ae28";
const TRAIN_SHA: &str = "023d6e5428236502e877bac0afe0e9e9ee4aa9d10ab38eb940ca8e915cf7502e";
const V6_MANIFEST_SHA: &str = "9645a48e7edc67f7a9e28f711a628cc47cdcf2db5245fffade4cba4b8191501e";
const REGISTRY_SHA: &str = "2b5fc26d0de6593745f1eb7e4cac4e611173107a7dc2c0bbd0bb0ef0416b5daa";
const OLD_SUMS_SHA: &str = "68d7cd4e9c1660cd839f657a07b4c70c8c3dac1e4f5e0eed838020dc3b9f77e0";
const PREVIOUS: &str = "datasets/a-balanced-20261026-v1";
// Keep the older publishers' guards unchanged. These structural projection and
// byte-span checks are intentionally mirrored; this release has separate pins.
pub(crate) fn sha(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}
pub(crate) fn s<'a>(v: &'a Value, k: &str) -> Result<&'a str> {
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
pub(crate) fn read_local(root: &Path, relative: &str) -> Result<Vec<u8>> {
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
pub(crate) fn load(root: &Path, relative: &str, expected: Option<&str>) -> Result<Value> {
    let bytes = read_local(root, relative)?;
    if let Some(expected) = expected {
        ensure!(sha(&bytes) == expected, "input digest differs: {relative}");
    }
    Ok(serde_json::from_slice(&bytes)?)
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

pub(crate) struct Generator {
    pub(crate) slot: String,
    pub(crate) config: Value,
    pub(crate) config_sha: String,
    pub(crate) registry: Value,
    pub(crate) binding: Value,
}
#[derive(Clone, Copy)]
pub(crate) enum GenerationRecipe {
    TrainScale,
    BookV2,
}
fn project_generation(
    row: &Value,
    generator: &Generator,
    cohort: &Path,
    evidence: &BTreeMap<String, String>,
) -> Result<Value> {
    project_generation_scoped(
        row,
        generator,
        cohort,
        evidence,
        "generation",
        GenerationRecipe::TrainScale,
    )
}
// Historical entry point above keeps its exact V3 recipe and archive layout.
// The retained-book publication separately pins its original V2 generation.
pub(crate) fn project_generation_scoped(
    row: &Value,
    generator: &Generator,
    cohort: &Path,
    evidence: &BTreeMap<String, String>,
    directory: &str,
    recipe: GenerationRecipe,
) -> Result<Value> {
    ensure!(
        ["", "generation"].contains(&directory),
        "unsupported generation directory"
    );
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
            "recipe",
            "scratch_policy",
            "rewrite_policy",
        ],
        &["generator_config", "endpoint"],
    )?;
    generation_contract(b, recipe)?;
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
    let prefix = if directory.is_empty() {
        format!("{}/{id}/", generator.slot)
    } else {
        format!("{directory}/{}/{id}/", generator.slot)
    };
    let relative = format!("{prefix}context.json");
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
fn generation_contract(b: &Value, recipe: GenerationRecipe) -> Result<()> {
    ensure!(
        b["thinking"] == false && b["generator_config"]["allowed_splits"] == json!(["train"]),
        "generation Train scope"
    );
    ensure!(
        match recipe {
            GenerationRecipe::TrainScale =>
                b["schema"] == "slop_ninja_controlled_generation_v3"
                    && b["recipe"] == "train_scale_v1"
                    && b["requested_span_percentages"] == json!([10, 40, 80, 100]),
            GenerationRecipe::BookV2 =>
                b["schema"] == "slop_ninja_controlled_generation_v2"
                    && b.get("recipe").is_none()
                    && b["requested_span_percentages"] == json!([20, 60, 100]),
        },
        "generation recipe differs"
    );
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

fn train_scope(row: &Value) -> Result<()> {
    ensure!(
        row["schema"] == "slop_ninja_a_supervised_record_v1"
            && row["frozen_split"] == "train"
            && row["purpose"] == "query"
            && row["documented_human_only"] == false
            && row["human_identity"].is_null()
            && row["grammar_exposure"] == "unknown"
            && row["exposed_to_grammar"].is_null(),
        "Train proxy/unknown identity/exposure boundary"
    );
    ensure!(
        row["text_sha256"] == sha(s(row, "text")?.as_bytes()),
        "text digest differs"
    );
    for k in [
        "commercial_training",
        "model_release",
        "external_evaluation",
        "redistribute_text",
    ] {
        ensure!(row["rights"][k] == true, "source right denied: {k}");
    }
    validate_targets(row)?;
    supervision::Supervision::parse(row)?;
    Ok(())
}

pub(crate) fn bind_original(root: &Value, original: &Value, line_hash: &str) -> Result<()> {
    train_scope(root)?;
    ensure!(
        root["origin"] == "human_only"
            && root["evidence"] == "historical_proxy"
            && root["generation"].is_null()
            && root["parent_id"].is_null()
            && root["share_target"] == 0.0,
        "not an unchanged human root"
    );
    let src = &root["source"];
    let mut recovered = src["record"].clone();
    recovered["text"] = root["text"].clone();
    ensure!(
        &recovered == original,
        "root history differs from published original Train record"
    );
    ensure!(
        original["schema"] == "slop-ninja-origin-record-v1"
            && original["split"] == "train"
            && original["origin"] == "human_only"
            && original["generation"].is_null()
            && original["parent_id"].is_null()
            && original["evidence"] == "historical_proxy"
            && original["source_group"] == root["source_group"]
            && original["rights"] == root["rights"]
            && original["source"]["collection"] == root["metadata"]["source_collection"]
            && original["id"] == root["metadata"]["original_record_id"],
        "original history/rights mismatch"
    );
    ensure!(
        src["original_train_sha256"] == TRAIN_SHA
            && src["source_manifest_sha256"] == V6_MANIFEST_SHA
            && src["original_jsonl_line_sha256"] == line_hash
            && src["original_text_sha256"] == root["text_sha256"]
            && src["byte_span"] == json!([0, s(root, "text")?.len()])
            && src["extraction"] == "unchanged_whole_original_v6_train_human_record_v1",
        "original file/line/text/span binding differs"
    );
    ensure!(
        (150..=450).contains(&s(root, "text")?.split_whitespace().count()),
        "root length"
    );
    Ok(())
}

fn bind_parent(row: &Value, parent: &Value) -> Result<()> {
    train_scope(row)?;
    ensure!(
        parent["origin"] == "human_only"
            && parent["generation"].is_null()
            && row["parent_id"] == parent["id"],
        "missing human parent"
    );
    for k in [
        "source",
        "rights",
        "metadata",
        "source_group",
        "frozen_split",
        "purpose",
    ] {
        ensure!(row[k] == parent[k], "descendant changed inherited {k}");
    }
    ensure!(
        row["share_evidence"]["source_text"] == parent["text"]
            && row["share_evidence"]["source_sha256"] == parent["text_sha256"],
        "parent bytes differ"
    );
    let p = row["share_evidence"]["requested_percent_not_target"]
        .as_u64()
        .context("percentage")?;
    ensure!(
        [0, 10, 40, 80, 100].contains(&p)
            && row["origin"] == if p == 0 { "model_only" } else { "mixed" }
            && row["share_evidence"]["full_rewrite_is_mixed_history"] == (p == 100),
        "workflow label differs"
    );
    ensure!(
        row["evidence"]
            == if p == 0 {
                "recorded_local_generation_from_scratch"
            } else {
                "recorded_local_rewrite_of_historical_human_proxy"
            },
        "workflow evidence differs"
    );
    Ok(())
}

pub(crate) fn project_source(source: &Value) -> Result<Value> {
    let mut out = flat(
        source,
        &[
            "byte_span",
            "extraction",
            "original_jsonl_line_sha256",
            "original_text_sha256",
            "original_train_sha256",
            "source_manifest_sha256",
        ],
        &["record"],
    )?;
    let r = &source["record"];
    let mut record = flat(
        r,
        &[
            "schema",
            "id",
            "text_sha256",
            "source_group",
            "split",
            "origin",
            "evidence",
            "evidence_notes",
            "parent_id",
            "generation",
        ],
        &["source", "rights"],
    )?;
    record["source"] = flat(
        &r["source"],
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
    record["rights"] = public_rights(&r["rights"])?;
    out["record"] = record;
    out["published_file"] = "datasets/detector-v6/train.jsonl".into();
    out["original_line_checksum_basis"] =
        "exact original JSONL line bytes excluding terminating newline".into();
    Ok(out)
}

fn project_row(
    row: &Value,
    line_hash: &str,
    scale: &Path,
    generators: &[Generator],
    evidence: &BTreeMap<String, String>,
) -> Result<Value> {
    train_scope(row)?;
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
        &[
            "source",
            "rights",
            "metadata",
            "share_evidence",
            "generation",
        ],
    )?;
    out["metadata"] = flat(
        &row["metadata"],
        &[
            "source_collection",
            "original_record_id",
            "word_count",
            "selection_rank_sha256",
            "v6_upstream_exposure",
            "historical_date",
            "historical_date_basis",
        ],
        &[],
    )?;
    out["source"] = project_source(&row["source"])?;
    out["rights"] = public_rights(&row["rights"])?;
    out["share_evidence"] = row["share_evidence"].clone();
    out["generation"] = if row["generation"].is_null() {
        Value::Null
    } else {
        let g = generators
            .iter()
            .find(|g| row["ai_identity"] == g.config["identity"])
            .context("unqualified generator")?;
        project_generation(row, g, scale, evidence)?
    };
    out["publication"] = json!({"original_line_sha256":line_hash,
        "line_hash_basis":"exact assembly JSONL line including terminating newline",
        "original_row_sha256":compact_sha(row)?,"row_hash_basis":"serde_json compact Value encoding",
        "projection":"named filesystem locators/endpoints omitted; exact prose, spans, targets, source rights and history retained",
        "source_rights_are_ancestral":true,"curation_and_generated_text_license":"CC-BY-SA-4.0 to the extent rights exist, subject to underlying rights and generator terms",
        "changes":if row["generation"].is_null() {"Original Train root text unchanged"} else {"Recorded model scratch/rewrite; see exact source/replacement spans and generation history"},
        "public_exposed_population":true,"future_hidden_test":false});
    public_safety(&out)?;
    Ok(out)
}

fn evidence_projection(index: &Value, scale: &Path) -> Result<(Value, BTreeMap<String, String>)> {
    let mut public = Vec::new();
    let mut map = BTreeMap::new();
    let mut tasks = BTreeSet::new();
    for item in array(index, "inputs")? {
        if let Some(path) = item["path"].as_str() {
            flat(item, &["path", "sha256"], &[])?;
            digest(item, "sha256")?;
            let relative = Path::new(path)
                .strip_prefix(scale)
                .context("outside assembly archive")?
                .to_str()
                .context("UTF8 archive name")?;
            let public_name = match relative {
                "../new-cohort/dataset-v1/manifest.json" => "protected/queries/manifest.json",
                "../new-cohort/dataset-v1/records.jsonl" => "protected/queries/records.jsonl",
                "../new-cohort/partition-references-v1/input/manifest.json" => {
                    "protected/references/manifest.json"
                }
                "../new-cohort/partition-references-v1/input/records.jsonl" => {
                    "protected/references/records.jsonl"
                }
                name => {
                    ensure!(
                        Path::new(name)
                            .components()
                            .all(|c| matches!(c, Component::Normal(_))),
                        "unsafe archive reference"
                    );
                    name
                }
            };
            let hash = s(item, "sha256")?.to_owned();
            if let Some(old) = map.insert(relative.to_owned(), hash.clone()) {
                ensure!(old == hash, "conflicting evidence hash");
            }
            public.push(json!({"archive_reference":public_name,"sha256":hash}));
        } else {
            let task = flat(
                item,
                &[
                    "task_id",
                    "intent_count",
                    "response_count",
                    "uncertain_attempt_indices",
                ],
                &[],
            )?;
            ensure!(
                tasks.insert(s(item, "task_id")?.to_owned()),
                "duplicate task accounting"
            );
            public.push(task);
        }
    }
    Ok((
        json!({"schema":"slop_ninja_train_public_evidence_index_v1","entries":public,
        "archive_references_are_not_distributed_files":true,"raw_invocations_retained_privately":true}),
        map,
    ))
}

fn previous_checksums(repo: &Path) -> Result<()> {
    let old = repo.join(PREVIOUS);
    let bytes = read_local(&old, "SHA256SUMS")?;
    ensure!(
        sha(&bytes) == OLD_SUMS_SHA,
        "previous snapshot checksum index changed"
    );
    for line in std::str::from_utf8(&bytes)?.lines() {
        let (expected, name) = line.split_once("  ").context("checksum format")?;
        ensure!(
            !name.to_lowercase().contains("test"),
            "unexpected Test checksum input"
        );
        ensure!(
            sha(&read_local(&old, name)?) == expected,
            "previous snapshot changed: {name}"
        );
    }
    Ok(())
}

fn run(repo: &Path, scale: &Path, out: &Path) -> Result<Value> {
    ensure!(!out.exists(), "new snapshot directory required");
    let repo = repo.canonicalize()?;
    let scale = scale.canonicalize()?;
    ensure!(
        out.parent().context("output parent")?.canonicalize()?
            == repo.join("datasets").canonicalize()?,
        "output must be direct datasets child"
    );
    let name = out
        .file_name()
        .context("output name")?
        .to_str()
        .context("UTF8 output")?;
    ensure!(
        name.starts_with("a-train-augmentation-")
            && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-'),
        "snapshot name"
    );
    previous_checksums(&repo)?;
    let m = load(&scale, "dataset-v1/manifest.json", Some(MANIFEST_SHA))?;
    ensure!(
        m["complete"] == true
            && m["train_extension_only"] == true
            && m["test_text_opened"] == false
            && array(&m, "test_ids")?.is_empty()
            && m["split_assignments"]
                .as_object()
                .context("splits")?
                .values()
                .all(|v| v == "train"),
        "Train manifest scope"
    );
    let mb = serde_json::to_vec(&m)?;
    let contract = supervision::Dataset::parse(&mb)?;
    let bytes = read_local(&scale, "dataset-v1/records.jsonl")?;
    ensure!(
        sha(&bytes) == s(&m, "records_sha256")?,
        "assembly records changed"
    );
    let mut rows = BTreeMap::new();
    let mut line_hashes = BTreeMap::new();
    for line in bytes.split_inclusive(|b| *b == b'\n') {
        ensure!(line.last() == Some(&b'\n'), "final newline required");
        let row: Value = serde_json::from_slice(line)?;
        let id = s(&row, "id")?.to_owned();
        contract.validate_metadata(&id, &row)?;
        train_scope(&row)?;
        line_hashes.insert(id.clone(), sha(line));
        ensure!(rows.insert(id, row).is_none(), "duplicate row ID");
    }
    contract.require_exact_ids(rows.keys().map(String::as_str))?;
    let roots = rows
        .values()
        .filter(|r| r["generation"].is_null())
        .map(|r| Ok((s(&r["source"]["record"], "id")?.to_owned(), r)))
        .collect::<Result<BTreeMap<_, _>>>()?;
    let original = read_local(&repo, "datasets/detector-v6/train.jsonl")?;
    ensure!(sha(&original) == TRAIN_SHA, "published V6 Train differs");
    ensure!(
        sha(&read_local(&repo, "datasets/detector-v6/manifest.json")?) == V6_MANIFEST_SHA,
        "V6 manifest differs"
    );
    let mut found = BTreeSet::new();
    for line in std::str::from_utf8(&original)?.lines() {
        let r: Value = serde_json::from_str(line)?;
        if let Some(root) = roots.get(s(&r, "id")?) {
            bind_original(root, &r, &sha(line.as_bytes()))?;
            ensure!(
                found.insert(s(&r, "id")?.to_owned()),
                "duplicate original ID"
            );
        }
    }
    ensure!(
        found.len() == roots.len()
            && roots.len()
                == m["requested_human_records"]
                    .as_u64()
                    .context("root count")? as usize,
        "missing original roots"
    );
    for row in rows.values().filter(|r| !r["generation"].is_null()) {
        bind_parent(
            row,
            rows.get(s(row, "parent_id")?).context("missing parent")?,
        )?;
    }
    let audit = load(
        &scale,
        "dataset-v1/audit.json",
        Some(s(&m, "audit_sha256")?),
    )?;
    let index = load(
        &scale,
        "dataset-v1/evidence-index.json",
        Some(s(&m["sources"][2], "sha256")?),
    )?;
    let (public_index, evidence) = evidence_projection(&index, &scale)?;
    let plan = load(
        &scale,
        "assembly-plan-v1.json",
        Some(s(&m["sources"][0], "sha256")?),
    )?;
    let registry = load(
        &repo,
        &format!("{PREVIOUS}/generators.json"),
        Some(REGISTRY_SHA),
    )?;
    let root_manifest = load(
        &scale,
        "roots-v1/manifest.json",
        Some(s(&plan, "roots_manifest_sha256")?),
    )?;
    ensure!(
        sha(&read_local(&scale, "roots-v1/humans.jsonl")?) == s(&plan, "roots_records_sha256")?,
        "root record hash"
    );
    let protocol = read_local(&scale, "ROOT_SELECTION_PROTOCOL.md")?;
    ensure!(
        sha(&protocol) == s(&m, "root_selection_protocol_sha256")?,
        "selection protocol changed"
    );
    public_safety(&json!(std::str::from_utf8(&protocol)?))?;
    let mut generators = Vec::new();
    let mut public_generators = Vec::new();
    for spec in array(&plan, "generators")? {
        let slot = s(spec, "slot")?;
        ensure!(
            ["known_model_1", "known_model_2"].contains(&slot),
            "unqualified model slot"
        );
        let config = load(
            &scale,
            s(spec, "config_file")?,
            Some(s(spec, "config_sha256")?),
        )?;
        ensure!(
            config["allowed_splits"] == json!(["train"]),
            "config Train scope"
        );
        let binding_path = format!("generation/{slot}/binding.json");
        let binding = load(
            &scale,
            &binding_path,
            Some(evidence.get(&binding_path).context("binding evidence")?),
        )?;
        let report = array(&m, "generation_reports")?
            .iter()
            .find(|r| r["slot"] == slot)
            .context("missing report")?;
        ensure!(
            compact_sha(&binding)? == s(report, "binding_sha256")?
                && report["complete"] == true
                && report["config_sha256"] == spec["config_sha256"]
                && report["generator"] == config["identity"],
            "report/config binding"
        );
        let old = array(&registry, "generators")?
            .iter()
            .find(|g| g["configuration"]["identity"] == config["identity"])
            .context("missing model license")?;
        let model = registry_model(&old["model"])?;
        let pc = project_config(&config)?;
        for k in ["filename", "sha256", "revision", "size_bytes"] {
            ensure!(pc[k] == model[k], "quantizer identity differs");
        }
        ensure!(
            pc["repo"] == model["quantized_repo"]
                && pc["rights"]["model_license"] == model["original_license"],
            "model licensing differs"
        );
        public_generators.push(json!({"configuration":pc,"model":model,"original_config_sha256":spec["config_sha256"],
            "original_binding_sha256":report["binding_sha256"],"producer_sha256":binding["producer_sha256"]}));
        generators.push(Generator {
            slot: slot.into(),
            config,
            config_sha: s(spec, "config_sha256")?.into(),
            registry: model,
            binding,
        });
    }
    let mut output = Vec::new();
    let mut collections = BTreeMap::<String, usize>::new();
    for (id, row) in &rows {
        let projected = project_row(row, &line_hashes[id], &scale, &generators, &evidence)
            .with_context(|| format!("project {id}"))?;
        // These are exact training inputs/targets; publication must not alter them.
        for k in [
            "id",
            "text",
            "text_sha256",
            "origin",
            "share_target",
            "share_evidence",
            "human_identity",
            "human_role_present",
            "ai_identity",
            "ai_role_present",
            "metadata",
        ] {
            ensure!(projected[k] == row[k], "projection changed {k}");
        }
        if row["origin"] == "human_only" {
            *collections
                .entry(s(&row["metadata"], "source_collection")?.into())
                .or_default() += 1;
        }
        serde_json::to_writer(&mut output, &projected)?;
        output.push(b'\n');
    }
    let mut public_audit = fields(
        &audit,
        &[
            "schema",
            "requested_roots",
            "requested_generation_tasks",
            "accepted_before_exclusions",
            "included",
            "excluded",
            "generators",
            "origins",
            "ai_identities",
            "unknown_human_identity_rows",
            "full_rewrite_mixed_share_one_rows",
            "test_text_opened",
            "evidence_files_modified",
        ],
        &["protected_populations"],
    )?;
    public_audit["protected_populations"] = array(&audit, "protected_populations")?
        .iter()
        .map(|v| {
            flat(
                v,
                &["manifest_sha256", "records_sha256", "records"],
                &["directory"],
            )
        })
        .collect::<Result<Vec<_>>>()?
        .into();
    public_audit["original_audit_sha256"] = m["audit_sha256"].clone();
    public_audit["selected_roots_by_collection"] = json!(collections);
    public_audit["root_selection_counts"] = root_manifest["collections"].clone();
    let public_generators = json!({"schema":"slop_ninja_train_augmentation_generators_v1","generators":public_generators,
        "runtime":registry["runtime"],"model_weights_included":false,"original_public_registry_sha256":REGISTRY_SHA});
    let mut files = BTreeMap::<String, Vec<u8>>::new();
    files.insert("records.jsonl".into(), output);
    for (name, v) in [
        ("audit.json", public_audit),
        ("evidence-index.json", public_index),
        ("generators.json", public_generators),
    ] {
        public_safety(&v)?;
        files.insert(name.into(), serde_json::to_vec_pretty(&v)?);
    }
    files.insert("ROOT_SELECTION_PROTOCOL.md".into(), protocol);
    files.insert("NOTICE.md".into(), NOTICE.as_bytes().to_vec());
    files.insert("LICENSES.md".into(), b"# License links\n\nOriginal sources retain their own licenses, attribution and notices in each row:\n\n- PLOS ONE: https://creativecommons.org/licenses/by/4.0/\n- Wikinews: https://creativecommons.org/licenses/by/2.5/\n- Historical Wikipedia: https://creativecommons.org/licenses/by-sa/3.0/\n- Additional CMU collection notice: https://creativecommons.org/licenses/by-sa/3.0/us/\n\nSlop Ninja curation and new generated text, to the extent rights exist and subject to underlying rights and generator terms: https://creativecommons.org/licenses/by-sa/4.0/ . Preserve attribution to Slop Ninja and the original contributors, per-source notices, and recorded changes. No warranties. Model licenses are separately retained in licenses/ and qualified in NOTICE.md and generators.json.\n".to_vec());
    files.insert("README.md".into(), README.as_bytes().to_vec());
    for (name, hash) in [
        (
            "qwen-base.LICENSE",
            "50cbab8a892c5f2993b8c7351a99182507472def3b1374558308605d99b86b32",
        ),
        (
            "phi-base.LICENSE",
            "fa8235e5b48faca34e3ca98cf4f694ef08bd216d28b58071a1f85b1d50cb814d",
        ),
    ] {
        let b = read_local(&repo, &format!("{PREVIOUS}/licenses/{name}"))?;
        ensure!(sha(&b) == hash, "model notice changed");
        files.insert(format!("licenses/{name}"), b);
    }
    let mut manifest = fields(
        &m,
        &[
            "schema",
            "cohort",
            "records",
            "non_test_ids",
            "test_ids",
            "split_assignments",
            "source_families",
            "generation_reports",
            "requested_human_records",
            "requested_generation_records",
            "requested_records",
            "accepted_before_exclusions",
            "excluded",
            "complete",
            "test_text_opened",
            "clean_holdout",
            "train_extension_only",
            "reference_policy",
            "rights_scope",
            "root_selection_protocol_sha256",
            "labels_are_not_inputs",
            "implementation_sha256",
            "supervision_implementation_sha256",
            "source_manifest_sha256",
        ],
        &["records_sha256", "input_sha256", "audit_sha256", "sources"],
    )?;
    manifest["records_file"] = "records.jsonl".into();
    manifest["records_sha256"] = sha(&files["records.jsonl"]).into();
    manifest["input_sha256"] = manifest["records_sha256"].clone();
    manifest["audit_sha256"] = sha(&files["audit.json"]).into();
    manifest["sources"] = json!([{"published_file":"datasets/detector-v6/train.jsonl","sha256":TRAIN_SHA},
        {"original_assembly_manifest_sha256":MANIFEST_SHA,"original_records_sha256":m["records_sha256"],"assembly_plan_sha256":m["sources"][0]["sha256"]},
        {"evidence_index":"evidence-index.json","sha256":sha(&files["evidence-index.json"]),"original_index_sha256":m["sources"][2]["sha256"]}]);
    manifest["publication"] = json!({"schema":"slop_ninja_train_augmentation_publication_v1","publisher_sha256":sha(include_bytes!("publish_train_augmentation.rs")),
        "previous_snapshot":PREVIOUS,"previous_snapshot_checksums_sha256":OLD_SUMS_SHA,"previous_snapshot_modified":false,
        "append_only_train_extension":true,"raw_invocations_included":false,"model_weights_included":false,
        "historical_proxy_not_documented_human_only":true,"v6_original_train_exposure":true,
        "prior_v6_mistral_exposure_not_undone":true,"no_new_mistral_or_gemma_generation":true});
    manifest["files"] = json!(
        files
            .iter()
            .map(|(n, b)| json!({"file":n,"bytes":b.len(),"sha256":sha(b)}))
            .collect::<Vec<_>>()
    );
    public_safety(&manifest)?;
    let manifest_bytes = serde_json::to_vec_pretty(&manifest)?;
    supervision::Dataset::parse(&manifest_bytes)?;
    files.insert("manifest.json".into(), manifest_bytes);
    let sums = files
        .iter()
        .map(|(n, b)| format!("{}  {n}\n", sha(b)))
        .collect::<String>();
    files.insert("SHA256SUMS".into(), sums.into_bytes());
    previous_checksums(&repo)?;
    fs::create_dir(out)?;
    fs::create_dir(out.join("licenses"))?;
    for (name, b) in &files {
        put(&out.join(name), b)?;
    }
    fs::File::open(out)?.sync_all()?;
    let result = json!({"records":rows.len(),"source_families":roots.len(),"records_sha256":manifest["records_sha256"],
        "manifest_sha256":sha(&files["manifest.json"]),"previous_snapshot_verified_unchanged":true,"test_text_opened":false});
    Ok(result)
}

const NOTICE: &str = "# Slop Ninja source and generator notices\n\nOriginal source rights remain in each row, including attribution, evidence hashes, URLs, contributor histories, and ShareAlike notices. PLOS ONE is CC BY 4.0; Wikinews is CC BY 2.5; historical Wikipedia text is CC BY-SA 3.0 Unported. CMU Book Summaries additionally credits David Bamman and Noah Smith (2013) under its CC BY-SA 3.0 United States collection notice. Book authors are not asserted to be the authors of summaries. Retain the exact per-source notices and changes.\n\nOriginal root text is unchanged. Descendants identify scratch versus controlled rewrite, with exact source/replacement spans and model history. Source rights are ancestral and do not certify human-only production or model-output factual accuracy. Historical human proxy labels remain qualified. New generated text and curation are CC BY-SA 4.0 to the extent rights exist, subject to underlying rights and generator terms; attribution: Slop Ninja and the recorded source contributors. The project's detector-weight release policy remains CC BY-SA 4.0, retaining original/base notices. This policy does not replace underlying source licenses.\n\nQwen3.6-35B-A3B original model: Apache-2.0 (licenses/qwen-base.LICENSE). Unsloth's quantizer card independently declares Apache-2.0. Phi-4-mini-instruct original model: MIT (licenses/phi-base.LICENSE); Bartowski's quantizer card states no independent license. Original metadata revisions are pinned independently of quantizer revisions and are not proven conversion inputs. Runtime, model and config hashes remain; weights and binaries are not included.\n";
const README: &str = "# Slop Ninja Train augmentation, 2026-10-26 v1\n\nAppend-only snapshot: 1,049 Train queries from 96 original V6 Train works, with 24 roots each from cmu-books-historical, hc3-wiki-historical, plos_one and wikinews. Keep a-balanced-20261026-v1 and every earlier dataset unchanged. This is an extension, not a replacement or a new evaluation set. records.jsonl plus manifest.json follow the standard supervised dataset contract.\n\nThe 1,056 candidates comprised 96 historical human proxies and 960 generated tasks (96 roots x two known models x scratch/10/40/80/100). Both generators accepted 480, with zero terminal failures and 960 retained intents/responses. Seven generated rows were excluded: six normalized duplicates and one no-change match to a human root. The retained rows are 96 human_only, 188 model_only scratch and 765 mixed-history rewrites. All 192 full rewrites retain mixed origin, human-role presence and measured final-word share 1. Requested percentages are not share targets. Unknown individual human identity is null; historical proxy is not documented contemporary human-only production.\n\nROOT_SELECTION_PROTOCOL.md fixes 150..450 whole-record words, one root per work and hash ordering, with no detector quality selection or quota substitution. The Wikinews historical-date field correction and preserved first selection are documented there. Original V6 Dev/CAL/Test families and all 72 fresh-cohort work metadata were protected; exact and normalized duplicate checks used the existing 929 non-Test queries and eight references. Alias and authorship metadata can be incomplete. No Test text was opened. Original V6 Train exposure and upstream Mistral training persist; excluding new Mistral/Gemma generation does not undo them. These 96 roots provide additional source works relative to the fresh pilot, not 1,049 independent works or a clean upstream holdout.\n\nAll exact text, targets, source bytes/spans, rights, original history and model/quantizer/runtime/sampler/prompt hashes are retained. Named machine paths and endpoints are projected out. Original JSONL line hashes exclude newline for V6 roots and include newline for assembly rows; see per-row publication fields. evidence-index.json contains hashes and logical archive references, including excluded tasks, not live invocation bodies. Raw responses and all attempts remain in the permanent ignored archive. See NOTICE.md and per-row source licensing.\n\nReproduce with the publish_train_augmentation Rust example: REPO TRAIN_SCALE NEW_DATASETS_CHILD. The frozen input hashes are compiled into this release qualifier. It validates the published V6 Train shard and complete assembly, then requires a new direct child of datasets. SHA256SUMS covers every distributed file except itself. No model calls, training, Test opening, or updates to an earlier snapshot occur.\n";

fn main() -> Result<()> {
    let a = std::env::args().skip(1).collect::<Vec<_>>();
    ensure!(
        a.len() == 3,
        "publish_train_augmentation REPO TRAIN_SCALE NEW_DATASETS_CHILD"
    );
    println!(
        "{}",
        run(Path::new(&a[0]), Path::new(&a[1]), Path::new(&a[2]))?
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn publication_cannot_relabel_generation_recipes() {
        let mut b = serde_json::json!({"schema":"slop_ninja_controlled_generation_v2","thinking":false,"generator_config":{"allowed_splits":["train"]},"requested_span_percentages":[20,60,100]});
        assert!(super::generation_contract(&b, super::GenerationRecipe::BookV2).is_ok());
        assert!(super::generation_contract(&b, super::GenerationRecipe::TrainScale).is_err());
        b["recipe"] = "train_scale_v1".into();
        assert!(super::generation_contract(&b, super::GenerationRecipe::BookV2).is_err());
        b["schema"] = "slop_ninja_controlled_generation_v3".into();
        b["requested_span_percentages"] = serde_json::json!([10, 40, 80, 100]);
        assert!(super::generation_contract(&b, super::GenerationRecipe::TrainScale).is_ok());
        b["generator_config"]["allowed_splits"] = serde_json::json!(["test"]);
        assert!(super::generation_contract(&b, super::GenerationRecipe::TrainScale).is_err());
    }
    use super::*;
    fn root() -> (Value, Value) {
        let text = (0..150).map(|_| "synthetic").collect::<Vec<_>>().join(" ");
        let rights = json!({"commercial_training":true,"redistribute_text":true,"model_release":true,
            "external_evaluation":true,"license":"CC-BY-4.0","attribution":"Synthetic fixture",
            "evidence_sha256":sha(b"rights"),"evidence_url":"https://example.org/license"});
        let original = json!({"schema":"slop-ninja-origin-record-v1","id":"original","split":"train",
            "origin":"human_only","source_group":"synthetic:work","text":text,"text_sha256":sha(text.as_bytes()),
            "parent_id":null,"generation":null,"evidence":"historical_proxy","evidence_notes":"Synthetic historical proxy",
            "rights":rights,"source":{"collection":"plos_one","url":"https://example.org/work","version":"1",
                "published_at":"2000-01-01","author_ids":["Synthetic contributors"],"raw_path":"/home/private/raw",
                "raw_sha256":sha(b"raw"),"extraction":"synthetic"}});
        let mut old = original.clone();
        old.as_object_mut().unwrap().remove("text");
        let row = json!({"schema":"slop_ninja_a_supervised_record_v1","id":"human","text":text,"text_sha256":sha(text.as_bytes()),
            "source_group":"synthetic:work","frozen_split":"train","purpose":"query","origin":"human_only",
            "evidence":"historical_proxy","documented_human_only":false,"human_identity":null,"human_role_present":true,
            "ai_identity":null,"ai_role_present":false,"parent_id":null,"generation":null,"share_target":0.0,
            "share_evidence":{"basis":"historical_human_proxy_endpoint","not_documented_contemporary_workflow":true},
            "grammar_exposure":"unknown","exposed_to_grammar":null,"rights":rights,
            "metadata":{"source_collection":"plos_one","original_record_id":"original"},
            "source":{"record":old,"original_jsonl_line_sha256":sha(b"exact original line"),"original_train_sha256":TRAIN_SHA,
                "source_manifest_sha256":V6_MANIFEST_SHA,"original_text_sha256":sha(text.as_bytes()),
                "byte_span":[0,text.len()],"extraction":"unchanged_whole_original_v6_train_human_record_v1"}});
        (row, original)
    }
    fn full_rewrite(parent: &Value) -> Value {
        let text = "New synthetic prose.";
        let mut r = parent.clone();
        r["id"] = "generated".into();
        r["parent_id"] = parent["id"].clone();
        r["origin"] = "mixed".into();
        r["ai_identity"] = "synthetic-model".into();
        r["ai_role_present"] = true.into();
        r["evidence"] = "recorded_local_rewrite_of_historical_human_proxy".into();
        r["text"] = text.into();
        r["text_sha256"] = sha(text.as_bytes()).into();
        r["share_target"] = 1.0.into();
        r["generation"] = json!({"mode":"controlled_rewrite"});
        r["share_evidence"] = json!({"schema":SPAN_SCHEMA,"source_text":parent["text"],"source_sha256":parent["text_sha256"],
            "source_byte_span":[0,parent["text"].as_str().unwrap().len()],"replacement":text,"final_byte_span":[0,text.len()],
            "final_sha256":sha(text.as_bytes()),"replacement_words":3,"final_words":3,"word_policy":"unicode_split_whitespace_v1",
            "realized_share":1.0,"requested_percent_not_target":100,"full_rewrite_is_mixed_history":true});
        r
    }
    #[test]
    fn original_binding_rejects_self_rehashed_history_and_line_changes() {
        let (mut r, o) = root();
        let line = sha(b"exact original line");
        bind_original(&r, &o, &line).unwrap();
        r["source"]["record"]["evidence_notes"] = "invented history".into();
        r["source"]["original_jsonl_line_sha256"] =
            compact_sha(&r["source"]["record"]).unwrap().into();
        assert!(bind_original(&r, &o, &line).is_err());
        let (mut r, o) = root();
        r["source"]["original_jsonl_line_sha256"] = sha(b"line\n").into();
        assert!(bind_original(&r, &o, &line).is_err());
    }
    #[test]
    fn train_scope_rejects_test_references_and_invented_identity() {
        let (r, _) = root();
        train_scope(&r).unwrap();
        for (k, v) in [
            ("frozen_split", json!("test")),
            ("frozen_split", json!("calibration")),
            ("purpose", json!("reference")),
            ("human_identity", json!("invented-writer")),
            ("documented_human_only", json!(true)),
        ] {
            let mut bad = r.clone();
            bad[k] = v;
            assert!(train_scope(&bad).is_err(), "{k}");
        }
    }
    #[test]
    fn full_rewrite_keeps_mixed_role_share_and_exact_parent() {
        let (p, _) = root();
        let r = full_rewrite(&p);
        bind_parent(&r, &p).unwrap();
        let mut bad = r.clone();
        bad["origin"] = "model_only".into();
        bad["human_role_present"] = false.into();
        assert!(bind_parent(&bad, &p).is_err());
        let mut bad = r.clone();
        bad["share_evidence"]["source_text"] = "self-rehashed replacement parent".into();
        bad["share_evidence"]["source_sha256"] = sha(b"self-rehashed replacement parent").into();
        assert!(bind_parent(&bad, &p).is_err());
        let mut bad = r;
        bad["source_group"] = "other-family".into();
        assert!(bind_parent(&bad, &p).is_err());
    }
    #[test]
    fn source_projection_preserves_rights_history_but_rejects_unknown_terms() {
        let (r, _) = root();
        let p = project_source(&r["source"]).unwrap();
        assert_eq!(p["record"]["rights"], r["rights"]);
        assert_eq!(
            p["record"]["evidence_notes"],
            r["source"]["record"]["evidence_notes"]
        );
        assert!(p["record"]["source"].get("raw_path").is_none());
        public_safety(&p).unwrap();
        let mut bad = r["source"].clone();
        bad["record"]["rights"]["new_restriction"] = "retain me".into();
        assert!(project_source(&bad).is_err());
    }
    #[test]
    fn evidence_projection_retains_attempt_hashes_without_private_locations() {
        let index = json!({"inputs":[{"path":"/fixture/generation/known_model_1/local-a/attempt-0.json","sha256":sha(b"response")},
            {"path":"/fixture/../new-cohort/dataset-v1/records.jsonl","sha256":sha(b"protected")},
            {"task_id":"local-a","intent_count":1,"response_count":1,"uncertain_attempt_indices":[]}]});
        let (p, map) = evidence_projection(&index, Path::new("/fixture")).unwrap();
        assert_eq!(
            map["generation/known_model_1/local-a/attempt-0.json"],
            sha(b"response")
        );
        assert_eq!(
            p["entries"][1]["archive_reference"],
            "protected/queries/records.jsonl"
        );
        assert_eq!(p["entries"][2]["response_count"], 1);
        public_safety(&p).unwrap();
        let bad = json!({"inputs":[{"path":"/fixture/../../secret","sha256":sha(b"x")}]});
        assert!(evidence_projection(&bad, Path::new("/fixture")).is_err());
    }
    #[test]
    fn public_safety_rejects_nested_paths_credentials_and_new_snapshot_overwrite() {
        for v in [
            json!({"nested":{"text":"/home/private/record"}}),
            json!({"url":"https://user:secret@example.org/"}),
            json!({"note":"api_key=secret"}),
        ] {
            assert!(public_safety(&v).is_err());
        }
        assert!(run(Path::new("."), Path::new("."), Path::new(".")).is_err());
    }
}
