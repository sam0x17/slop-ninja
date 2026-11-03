//! Pinned multi-generator experiment, with resumable exact span supervision.
//! Corpus/provenance outputs contain prose and belong in ignored task storage.
use anyhow::{Context, Result, ensure};
use reqwest::blocking::Client;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    io::Write,
    path::Path,
    time::{Duration, Instant},
};
#[path = "a_balanced_generate/encoder_recipe.rs"]
mod encoder_recipe;
#[allow(dead_code)]
#[path = "../grammar/crates/grammar-a-baseline/examples/freeze_last_block_test.rs"]
mod last_block_test_binding;
#[allow(dead_code)]
#[path = "a_balanced_generate/random_style_recipe.rs"]
mod random_style_recipe;
#[path = "a_balanced_generate/style_recipe.rs"]
mod style_recipe;

fn sha(b: &[u8]) -> String {
    hex::encode(Sha256::digest(b))
}
fn put(path: &Path, v: &Value) -> Result<()> {
    ensure!(!path.exists(), "refuse to replace evidence");
    let parent = path.parent().context("parent directory")?;
    let mut f = tempfile::NamedTempFile::new_in(parent)?;
    serde_json::to_writer_pretty(&mut f, v)?;
    f.write_all(b"\n")?;
    f.as_file().sync_all()?;
    f.persist_noclobber(path)?;
    fs::File::open(parent)?.sync_all()?;
    Ok(())
}
fn read(path: &Path) -> Result<Value> {
    Ok(serde_json::from_slice(&fs::read(path)?)?)
}
pub(crate) fn words(s: &str) -> usize {
    s.split_whitespace().count()
}
fn prose(content: &str) -> Result<&str> {
    let text = content.trim();
    // This runtime emits an empty reasoning wrapper with reasoning disabled.
    // Preserve the raw response and remove ONLY a verified empty leading block.
    if let Some(rest) = text.strip_prefix("<think>") {
        let (reasoning, body) = rest
            .split_once("</think>")
            .context("unclosed reasoning block")?;
        ensure!(
            reasoning.trim().is_empty(),
            "nonempty reasoning is not prose"
        );
        let body = body.trim();
        ensure!(!body.contains("<think>"), "nested reasoning");
        Ok(body)
    } else {
        ensure!(!text.contains("<think>"), "embedded reasoning");
        Ok(text)
    }
}

/// Span ends at a sentence boundary nearest the target, preserving original bytes.
/// Fallback to whitespace only when no sentence boundary exists. No token joins.
pub(crate) fn span(text: &str, percent: usize, position: usize) -> (usize, usize) {
    if percent == 100 {
        return (0, text.len());
    }
    let desired = (words(text) * percent / 100).max(1);
    let mut starts = vec![0];
    let mut boundaries = vec![text.len()];
    for (i, c) in text.char_indices() {
        if matches!(c, '.' | '?' | '!') {
            let end = i + c.len_utf8();
            if end == text.len() || text[end..].starts_with(char::is_whitespace) {
                boundaries.push(end);
                let next = end + text[end..].len() - text[end..].trim_start().len();
                if next < text.len() {
                    starts.push(next);
                }
            }
        }
    }
    if boundaries.len() <= 2 {
        for (i, c) in text.char_indices() {
            if c.is_whitespace() {
                boundaries.push(i);
                let next = i + text[i..].len() - text[i..].trim_start().len();
                if next < text.len() {
                    starts.push(next);
                }
            }
        }
    }
    let total = words(text);
    starts
        .iter()
        .flat_map(|&s| boundaries.iter().map(move |&e| (s, e)))
        .filter(|&(s, e)| s < e && (s > 0 || e < text.len()))
        .filter(|&(s, e)| match position {
            0 => s == 0,
            2 => e == text.len(),
            _ => true,
        })
        .min_by_key(|&(s, e)| {
            let count = words(&text[s..e]);
            let midpoint = 2 * words(&text[..s]) + count;
            (
                count.abs_diff(desired),
                if position == 1 {
                    midpoint.abs_diff(total)
                } else {
                    0
                },
                s,
                e,
            )
        })
        .unwrap_or((0, text.len()))
}

fn scratch_prompt(id: &str) -> String {
    let d = Sha256::digest(id.as_bytes());
    let places = [
        "a coastal village",
        "a railway town",
        "a remote farm",
        "a crowded city",
        "a mountain inn",
        "a small port",
        "a rural school",
        "a family home",
    ];
    let events = [
        "an unexpected visitor",
        "a missing letter",
        "a misunderstanding",
        "a difficult promise",
        "a sudden storm",
        "a secret discovered",
        "a change of plans",
        "a reunion",
    ];
    let objects = [
        "a broken clock",
        "an old map",
        "a lost key",
        "a worn coat",
        "a wooden box",
        "a faded photograph",
        "a borrowed bicycle",
        "a silver spoon",
    ];
    let roles = [
        "a gardener",
        "a teacher",
        "a shopkeeper",
        "a carpenter",
        "a musician",
        "a sailor",
        "a baker",
        "a librarian",
    ];
    let times = [
        "before dawn",
        "on a winter evening",
        "during a summer afternoon",
        "late at night",
        "on a rainy morning",
        "at the end of autumn",
        "at noon",
        "on a spring evening",
    ];
    let conflicts = [
        "an unwelcome obligation",
        "a hopeful invitation",
        "an uncertain memory",
        "a quiet rivalry",
        "a difficult departure",
        "a generous gesture",
        "a mistaken accusation",
        "a change of heart",
    ];
    format!(
        "Write an original English fictional scene of about 220 words set in {} involving {}, {}, {}, {}, and {}. Use natural narrative prose. Return only the scene, with no title, analysis, introduction, or explanation.",
        places[d[0] as usize % 8],
        events[d[1] as usize % 8],
        objects[d[2] as usize % 8],
        roles[d[3] as usize % 8],
        times[d[4] as usize % 8],
        conflicts[d[5] as usize % 8]
    )
}

// Versioned Train-only expansion. Prompts use collection-level register and a
// deterministic semantic topic, never a byline, source title, split or ID.
fn scale_scratch_prompt(source: &Value) -> Result<String> {
    let id = source["id"].as_str().context("source id")?;
    let d = Sha256::digest(id.as_bytes());
    let topic = [
        "water conservation",
        "public transport",
        "urban gardens",
        "renewable energy",
        "local libraries",
        "coastal ecology",
        "community health",
        "rural education",
    ][d[0] as usize % 8];
    let aspect = [
        "seasonal variation",
        "public participation",
        "long-term maintenance",
        "accessibility",
        "resource allocation",
        "local cooperation",
        "measurement reliability",
        "unexpected disruption",
    ][d[1] as usize % 8];
    let genre = match source["metadata"]["source_collection"].as_str() {
        Some("plos_one") => {
            "an abstract for a fictional scientific study, with its research question, methods, findings and limitations"
        }
        Some("wikinews") => {
            "a factual-style news report about an invented local event, with concrete details and reported perspectives"
        }
        Some("hc3-wiki-historical") => {
            "an encyclopedic explanation, with concrete details and clear relationships between ideas"
        }
        Some("cmu-books-historical") => {
            "a plot synopsis for an original fictional story, describing its main characters, events and outcome"
        }
        _ => anyhow::bail!("unqualified source collection for Train expansion"),
    };
    let setting = [
        "a small coastal town",
        "a rural district",
        "a large city",
        "an inland community",
        "a mountain region",
        "a university campus",
        "a river valley",
        "an island community",
    ][d[2] as usize % 8];
    Ok(format!(
        "Write {genre} in English, about {topic} and {aspect} in {setting}. Aim for 220 words. Return only the prose, without a title, preface, analysis, or explanation of the task."
    ))
}

fn generation_mode_args(a: &[String]) -> Result<(bool, bool)> {
    match a.get(3).map(String::as_str) {
        None if a.len() == 3 => Ok((false, false)),
        Some("--test") if a.len() == 4 => Ok((true, false)),
        Some("--recipe") if a.len() == 5 && a[4] == "train_scale_v1" => Ok((false, true)),
        Some("--recipe") if a.len() == 5 && a[4] == "encoder_stage3_v1" => Ok((false, false)),
        Some("--recipe") if a.len() == 6 && a[4] == "encoder_stage3_test_v1" => Ok((true, false)),
        Some("--recipe") if a.len() == 6 && a[4] == "style_diversity_v1" => Ok((false, false)),
        Some("--recipe") if a.len() == 6 && a[4] == random_style_recipe::VERSION => {
            Ok((false, false))
        }
        _ => anyhow::bail!(
            "a_balanced_generate HUMANS.jsonl OUTPUT GENERATOR.json [--test | --recipe train_scale_v1 | --recipe encoder_stage3_v1 | --recipe encoder_stage3_test_v1 SEAL.json | --recipe style_diversity_v1 FIX_SLOP_DIR | --recipe random_style_diversity_v1 CATALOG.json]"
        ),
    }
}

fn checked_config(config: &Value) -> Result<()> {
    ensure!(
        config["schema"] == "slop_ninja_local_generator_v1",
        "generator schema"
    );
    for field in [
        "identity",
        "repo",
        "revision",
        "filename",
        "sha256",
        "runtime_sha256",
        "model_path",
        "runtime_path",
        "alias",
    ] {
        ensure!(
            config[field].as_str().is_some_and(|s| !s.is_empty()),
            "missing generator {field}"
        );
    }
    for field in ["sha256", "runtime_sha256"] {
        let s = config[field].as_str().unwrap();
        ensure!(
            s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit()),
            "invalid digest"
        );
    }
    let endpoint = reqwest::Url::parse(config["endpoint"].as_str().context("endpoint")?)?;
    ensure!(
        endpoint.scheme() == "http"
            && endpoint.host_str() == Some("127.0.0.1")
            && endpoint.port().is_some()
            && endpoint.path() == "/"
            && endpoint.query().is_none()
            && endpoint.fragment().is_none()
            && endpoint.username().is_empty()
            && endpoint.password().is_none(),
        "task-owned loopback endpoint required"
    );
    ensure!(
        config["rights"]["generation_permitted"] == true,
        "generator rights not admitted"
    );
    let parts = config["allowed_splits"]
        .as_array()
        .context("allowed_splits")?;
    ensure!(
        !parts.is_empty()
            && parts.iter().all(|p| matches!(
                p.as_str(),
                Some("train" | "development" | "calibration" | "test")
            )),
        "invalid split scope"
    );
    Ok(())
}

fn file_sha(path: &Path) -> Result<String> {
    use std::io::Read;
    let mut f = fs::File::open(path)?;
    let mut h = Sha256::new();
    let mut buf = [0u8; 65536];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(hex::encode(h.finalize()))
}

pub(crate) fn assembled(
    source: &str,
    start: usize,
    end: usize,
    replacement: &str,
) -> Result<(String, usize, usize, f64)> {
    ensure!(
        source.is_char_boundary(start) && source.is_char_boundary(end) && start < end,
        "invalid source span"
    );
    ensure!(
        start == 0 || source[..start].ends_with(char::is_whitespace),
        "left token boundary"
    );
    ensure!(
        end == source.len() || source[end..].starts_with(char::is_whitespace),
        "right token boundary"
    );
    ensure!(
        !replacement.is_empty() && replacement.trim() == replacement,
        "replacement layout"
    );
    let text = format!("{}{}{}", &source[..start], replacement, &source[end..]);
    let final_end = start + replacement.len();
    let denominator = words(&text);
    let numerator = words(replacement);
    ensure!(
        denominator > 0 && numerator <= denominator,
        "word accounting"
    );
    Ok((
        text,
        start,
        final_end,
        numerator as f64 / denominator as f64,
    ))
}

fn main() -> Result<()> {
    let a = std::env::args().skip(1).collect::<Vec<_>>();
    let (test_mode, train_scale) = generation_mode_args(&a)?;
    let encoder_test = a.get(4).is_some_and(|s| s == "encoder_stage3_test_v1");
    let encoder_stage3 = encoder_test || a.get(4).is_some_and(|s| s == "encoder_stage3_v1");
    let random_style = a.get(4).is_some_and(|s| s == random_style_recipe::VERSION);
    let style_diversity = random_style || a.get(4).is_some_and(|s| s == "style_diversity_v1");
    let catalog = if random_style {
        read(Path::new(&a[5]))?
    } else {
        Value::Null
    };
    let skill = if style_diversity && !random_style {
        style_recipe::load_skill(Path::new(&a[5]))?
    } else {
        String::new()
    };
    let config_bytes = fs::read(&a[2])?;
    let config: Value = serde_json::from_slice(&config_bytes)?;
    checked_config(&config)?;
    if train_scale || style_diversity {
        ensure!(
            config["allowed_splits"] == json!(["train"]),
            "Train expansion requires Train-only generator scope"
        );
    }
    let model = config["identity"].as_str().unwrap();
    let endpoint = config["endpoint"].as_str().unwrap().trim_end_matches('/');
    ensure!(
        file_sha(Path::new(config["model_path"].as_str().unwrap()))? == config["sha256"],
        "model bytes differ"
    );
    ensure!(
        file_sha(Path::new(config["runtime_path"].as_str().unwrap()))? == config["runtime_sha256"],
        "runtime bytes differ"
    );
    for library in config["runtime_libraries"]
        .as_array()
        .context("runtime library bindings")?
    {
        ensure!(
            file_sha(Path::new(library["path"].as_str().context("library path")?))?
                == library["sha256"],
            "runtime library changed"
        );
    }
    let input = fs::read(&a[0])?;
    let test_binding = if encoder_test {
        let seal_path = Path::new(&a[5]);
        let selected = last_block_test_binding::validate(seal_path)?;
        let seal_hash = file_sha(seal_path)?;
        let admission_path = Path::new(&a[0])
            .parent()
            .context("admission parent")?
            .join("manifest.json");
        let admission = read(&admission_path)?;
        ensure!(
            admission["schema"] == "slop_ninja_last_block_test_admission_v1"
                && admission["complete"] == true
                && admission["test_used_for_selection"] == false
                && admission["selection_receipt_sha256"] == seal_hash
                && admission["files_sha256"]["humans.jsonl"] == sha(&input),
            "sealed Test admission required"
        );
        ensure!(
            selected["binding_files"]
                .as_array()
                .context("sealed files")?
                .iter()
                .any(|v| v["path"]
                    .as_str()
                    .is_some_and(|p| Path::new(p) == Path::new(&a[2]))
                    && v["sha256"] == sha(&config_bytes)),
            "generator configuration not sealed"
        );
        ensure!(
            config["allowed_splits"] == json!(["test"]),
            "Test-only generator required"
        );
        Some(
            json!({"selection_receipt_sha256":seal_hash,"admission_manifest_sha256":file_sha(&admission_path)?,
                "source_manifest_sha256":admission["source_manifest_sha256"]}),
        )
    } else {
        None
    };
    let out = Path::new(&a[1]);
    fs::create_dir_all(out)?;
    let mut binding = json!({"schema":"slop_ninja_controlled_generation_v2", "input_sha256":sha(&input),"generator":model,
        "generator_config":config,"generator_config_sha256":sha(&config_bytes),"producer_sha256":sha(include_bytes!("a_balanced_generate.rs")),
        "endpoint":endpoint, "share_word_policy":"unicode_split_whitespace_v1",
        "requested_span_percentages":[20,60,100],"span_policy":"nearest_sentence_start_middle_end_by_source_digest_v2", "seed":1729, "temperature":0.7, "top_p":0.9,
        "max_tokens":1024,"thinking":false,"input_schema":"slop_ninja_a_supervised_record_v1",
        "failures":"retained per attempt; maximum two requests per item; no substitute labels", "test_opened":test_mode});
    if train_scale {
        binding["schema"] = json!("slop_ninja_controlled_generation_v3");
        binding["recipe"] = json!("train_scale_v1");
        binding["requested_span_percentages"] = json!([10, 40, 80, 100]);
        binding["scratch_policy"] =
            json!("collection_register_and_independent_digest_selected_semantic_topic_v1");
        binding["rewrite_policy"] = json!("preserve_register_meaning_facts_perspective_length_v1");
    }
    if encoder_stage3 {
        binding["schema"] = json!("slop_ninja_controlled_generation_v4");
        binding["recipe"] = json!("encoder_stage3_v1");
        binding["requested_span_percentages"] = json!([10, 40, 80, 100]);
        binding["scratch_policy"] = json!("acquisition_stratum_independent_topic_v1");
        binding["scratch_implementation_sha256"] =
            json!(sha(include_bytes!("a_balanced_generate/encoder_recipe.rs")));
        binding["rewrite_policy"] = json!("preserve_register_meaning_facts_perspective_length_v1");
        binding["test_policy"] = json!(
            "Test unavailable in this recipe; separate frozen selection route required later"
        );
        if let Some(test_binding) = &test_binding {
            binding["test_policy"] = json!("frozen_last_block_Test_v1; no fitting or reselection");
            binding["test_binding"] = test_binding.clone();
        }
    }
    let binding_path = out.join("binding.json");
    if style_diversity {
        binding["schema"] = json!("slop_ninja_controlled_generation_v5");
        binding["recipe"] = json!("style_diversity_v1");
        binding["requested_span_percentages"] = json!([40, 100]);
        binding["instruction_matrix"] = style_recipe::contract();
        binding["instruction_matrix_producer_sha256"] =
            json!(sha(include_bytes!("a_balanced_generate/style_recipe.rs")));
        binding["skill_bundle_sha256"] = json!(sha(skill.as_bytes()));
        if random_style {
            ensure!(
                catalog["parents_sha256"] == sha(&input),
                "catalog source population differs"
            );
            binding["recipe"] = json!(random_style_recipe::VERSION);
            binding["instruction_matrix"] = catalog
                .get("instruction_matrix")
                .cloned()
                .unwrap_or_else(random_style_recipe::contract);
            binding["instruction_matrix_producer_sha256"] = json!(sha(include_bytes!(
                "a_balanced_generate/random_style_recipe.rs"
            )));
            binding["catalog_sha256"] = json!(sha(&fs::read(&a[5])?));
            binding["skill_bundle_sha256"] = catalog["fix_slop_bundle_sha256"].clone();
        }
    }
    if binding_path.exists() {
        ensure!(read(&binding_path)? == binding, "resume binding differs");
    } else {
        put(&binding_path, &binding)?;
    }
    let client = Client::builder()
        .timeout(Duration::from_secs(300))
        .redirect(reqwest::redirect::Policy::none())
        .retry(reqwest::retry::never())
        .build()?;
    let models: Value = client
        .get(format!("{endpoint}/v1/models"))
        .send()?
        .error_for_status()?
        .json()?;
    ensure!(
        models["data"]
            .as_array()
            .is_some_and(|a| a.iter().any(|m| m["id"] == config["alias"])),
        "server alias mismatch"
    );
    let props: Value = client
        .get(format!("{endpoint}/props"))
        .send()?
        .error_for_status()?
        .json()?;
    ensure!(
        props["model_path"] == config["model_path"],
        "server model path mismatch"
    );
    let props_path = out.join("server-properties.json");
    if !props_path.exists() {
        put(&props_path, &props)?;
    }
    let frozen_props = read(&props_path)?;
    for key in [
        "model_path",
        "chat_template",
        "total_slots",
        "default_generation_settings",
    ] {
        ensure!(
            props[key] == frozen_props[key],
            "server configuration changed: {key}"
        );
    }
    let mut prompt_splits = BTreeMap::new();
    for line in std::str::from_utf8(&input)?.lines() {
        let source: Value = serde_json::from_str(line)?;
        if source["purpose"] == "reference" {
            continue;
        }
        if train_scale || style_diversity {
            ensure!(
                source["frozen_split"] == "train" && source["purpose"] == "query",
                "Train expansion accepts only Train queries"
            );
        }
        if style_diversity {
            continue;
        }
        let prompt = if encoder_stage3 {
            encoder_recipe::prompt(&source)?
        } else if train_scale {
            scale_scratch_prompt(&source)?
        } else {
            scratch_prompt(source["id"].as_str().context("source id")?)
        };
        if let Some(prior) = prompt_splits.insert(prompt, source["frozen_split"].clone()) {
            ensure!(
                prior == source["frozen_split"],
                "scratch prompt crosses partitions"
            );
        }
    }
    let mut accepted = Vec::new();
    let mut failures = Vec::new();
    for line in std::str::from_utf8(&input)?.lines() {
        let source: Value = serde_json::from_str(line)?;
        ensure!(source["origin"] == "human_only", "human source required");
        if let Some(test_binding) = &test_binding {
            ensure!(
                source["source"]["source_manifest_sha256"]
                    == test_binding["source_manifest_sha256"],
                "Test source acquisition seal differs"
            );
        }
        ensure!(
            if test_mode {
                source["frozen_split"] == "test"
            } else {
                matches!(
                    source["frozen_split"].as_str(),
                    Some("train" | "development" | "calibration")
                )
            },
            "unauthorized population"
        );
        ensure!(
            source["rights"]["commercial_training"] == true,
            "training rights missing"
        );
        let text = source["text"].as_str().context("source text")?;
        ensure!(
            source["text_sha256"] == sha(text.as_bytes()),
            "source digest mismatch"
        );
        let id = source["id"].as_str().context("source id")?;
        if source["purpose"] == "reference" {
            continue;
        }
        if !config["allowed_splits"]
            .as_array()
            .unwrap()
            .contains(&source["frozen_split"])
        {
            continue;
        }
        let percentages: &[usize] = if train_scale || encoder_stage3 {
            &[0, 10, 40, 80, 100]
        } else {
            &[0, 20, 60, 100]
        };
        let cases = if random_style {
            random_style_recipe::cases(&catalog, id, model)?
        } else {
            style_recipe::cases(style_diversity, percentages)
                .into_iter()
                .map(|(s, p)| (s.to_owned(), p))
                .collect()
        };
        for (style, percent) in cases {
            let mode = if percent == 0 {
                "from_scratch"
            } else {
                "controlled_rewrite"
            };
            let generation_id = if random_style {
                format!(
                    "local-{}",
                    &sha(format!(
                        "{id}:{model}:{mode}:{percent}:{}:{style}",
                        random_style_recipe::VERSION
                    )
                    .as_bytes())[..24]
                )
            } else if style_diversity {
                format!(
                    "local-{}",
                    &sha(
                        format!("{id}:{model}:{mode}:{percent}:style-diversity-v1:{style}")
                            .as_bytes()
                    )[..24]
                )
            } else {
                format!(
                    "local-{}",
                    &sha(format!(
                        "{id}:{model}:{mode}:{percent}:{}",
                        if encoder_stage3 {
                            "span-v4-encoder-stage3"
                        } else if train_scale {
                            "span-v3-train-scale"
                        } else {
                            "span-v2"
                        }
                    )
                    .as_bytes())[..24]
                )
            };
            let dir = out.join(&generation_id);
            fs::create_dir_all(&dir)?;
            let done = dir.join("record.json");
            let position = Sha256::digest(format!("{id}:{percent}").as_bytes())[0] as usize % 3;
            let position_name = ["start", "middle", "end"][position];
            let (start, end) = if percent == 0 {
                (0, 0)
            } else {
                span(text, percent, position)
            };
            let prompt = if random_style {
                random_style_recipe::prompt(&catalog, &style, &text[start..end])?
            } else if style_diversity {
                style_recipe::prompt(&style, &text[start..end], &skill)?
            } else if percent == 0 {
                if encoder_stage3 {
                    encoder_recipe::prompt(&source)?
                } else if train_scale {
                    scale_scratch_prompt(&source)?
                } else {
                    scratch_prompt(id)
                }
            } else if train_scale || encoder_stage3 {
                format!(
                    "Rewrite the following English passage in your own words while preserving its meaning, facts, perspective, register, and approximate length. Return only the replacement prose, retaining paragraph breaks where useful: no title, preface, analysis, quotation delimiters, or explanation.\n\n{}",
                    &text[start..end]
                )
            } else {
                format!(
                    "Rewrite the following English narrative passage in your own words, keeping its events, perspective, and approximate length. Return only the replacement prose in one paragraph: no title, preface, analysis, quotation delimiters, or explanation.\n\n{}",
                    &text[start..end]
                )
            };
            let request = json!({"model":config["alias"],"messages":[{"role":"user","content":prompt}],
                "temperature":0.7,"top_p":0.9,"seed":1729,"max_tokens":1024,"stream":false,
                "chat_template_kwargs":{"enable_thinking":false},"reasoning_format":"none"});
            let mut plan = json!({"id":generation_id,"parent_id":id,"source_sha256":source["text_sha256"],"source_record_sha256":sha(&serde_json::to_vec(&source)?),"binding_sha256":sha(&serde_json::to_vec(&binding)?),"position":position_name,
                "mode":mode,"requested_source_span_percent":percent,"source_byte_span":[start,end],"request":request,
                "sham_reassembly_exact": percent==0 || format!("{}{}{}",&text[..start],&text[start..end],&text[end..])==text});
            if style_diversity {
                plan["instruction_style"] = json!(style);
            }
            let path = dir.join("plan.json");
            if path.exists() {
                ensure!(read(&path)? == plan, "generation plan differs");
            } else {
                put(&path, &plan)?;
            }
            let context_path = dir.join("context.json");
            if !context_path.exists() {
                let rendered: Value = client.post(format!("{endpoint}/apply-template")).json(&json!({"messages":request["messages"],"add_generation_prompt":true,"chat_template_kwargs":{"enable_thinking":false}})).send()?.error_for_status()?.json()?;
                let prompt = rendered["prompt"].as_str().context("rendered prompt")?;
                let tokens: Value = client
                    .post(format!("{endpoint}/tokenize"))
                    .json(&json!({"content":prompt,"add_special":false,"parse_special":true}))
                    .send()?
                    .error_for_status()?
                    .json()?;
                let count = tokens["tokens"].as_array().context("prompt tokens")?.len();
                put(
                    &context_path,
                    &json!({"request_sha256":sha(&serde_json::to_vec(&request)?),"rendered_prompt_sha256":sha(prompt.as_bytes()),"prompt_tokens":count,"max_output_tokens":1024,"context_capacity":8192,"fits":count+1024<8192}),
                )?;
            }
            let context = read(&context_path)?;
            ensure!(
                context["request_sha256"] == sha(&serde_json::to_vec(&request)?),
                "context receipt request mismatch"
            );
            if context["fits"] != true {
                failures.push(json!({"id":generation_id,"parent_id":id,"reason":"unsupported_context","context":context}));
                continue;
            }
            if done.exists() && dir.join("record.sha256.json").exists() {
                let r = read(&done)?;
                let evidence = read(&dir.join("record.sha256.json"))?;
                ensure!(
                    evidence["sha256"] == sha(&fs::read(&done)?)
                        && evidence["plan_sha256"] == sha(&fs::read(&path)?),
                    "resume record/plan digest differs"
                );
                ensure!(
                    r["parent_id"] == id
                        && r["ai_identity"] == model
                        && r["generation"]["binding"] == binding,
                    "resume record binding"
                );
                accepted.push(r);
                continue;
            }
            let mut result = None;
            for attempt in 0..2 {
                let path = dir.join(format!("attempt-{attempt}.json"));
                let response = if path.exists() {
                    read(&path)?
                } else {
                    let intent = dir.join(format!("attempt-{attempt}.intent.json"));
                    // An interrupted request is retained as uncertain, never silently replayed.
                    if intent.exists() {
                        continue;
                    }
                    put(
                        &intent,
                        &json!({"request":request,"plan_sha256":sha(&fs::read(dir.join("plan.json"))?),"attempt":attempt}),
                    )?;
                    let clock = Instant::now();
                    let r = client
                        .post(format!("{endpoint}/v1/chat/completions"))
                        .json(&request)
                        .send();
                    let v = match r {
                        Ok(response) => {
                            let status = response.status().as_u16();
                            match response.text() {
                                Ok(body) => {
                                    json!({"status":status,"raw_body":body,"body":serde_json::from_str::<Value>(&body).unwrap_or(Value::Null),"elapsed_ms":clock.elapsed().as_millis()})
                                }
                                Err(error) => {
                                    json!({"status":status,"body_read_error":error.to_string(),"elapsed_ms":clock.elapsed().as_millis()})
                                }
                            }
                        }
                        Err(error) => {
                            json!({"status":0,"error":error.to_string(),"elapsed_ms":clock.elapsed().as_millis()})
                        }
                    };
                    put(&path, &v)?;
                    v
                };
                let replacement = match prose(
                    response["body"]["choices"][0]["message"]["content"]
                        .as_str()
                        .unwrap_or(""),
                ) {
                    Ok(text) => text,
                    Err(_) => continue,
                };
                let finish = response["body"]["choices"][0]["finish_reason"].as_str();
                if response["status"] != 200
                    || finish != Some("stop")
                    || words(replacement) < 8
                    || replacement.contains("<think>")
                {
                    continue;
                }
                let (final_text, fs, fe, share) = if percent == 0 {
                    (replacement.to_owned(), 0, replacement.len(), 1.)
                } else {
                    assembled(text, start, end, replacement)?
                };
                let mut row = source.clone();
                row["id"] = generation_id.clone().into();
                row["text"] = final_text.clone().into();
                row["text_sha256"] = sha(final_text.as_bytes()).into();
                row["parent_id"] = id.into();
                row["origin"] = if percent == 0 { "model_only" } else { "mixed" }.into();
                row["evidence"] = if percent == 0 {
                    "recorded_local_generation_from_scratch"
                } else {
                    "recorded_local_rewrite_of_historical_human_proxy"
                }
                .into();
                row["human_role_present"] = (percent != 0).into();
                if percent == 0 {
                    row["human_identity"] = Value::Null;
                }
                row["ai_role_present"] = true.into();
                row["ai_identity"] = model.into();
                row["share_target"] = share.into();
                row["share_evidence"] = json!({"schema":"slop_ninja_realized_span_share_v1","source_text":text,
                    "source_sha256":source["text_sha256"],"source_byte_span":[start,end],"replacement":replacement,
                    "final_byte_span":[fs,fe],"final_sha256":row["text_sha256"],"replacement_words":words(replacement),
                    "final_words":words(&final_text),"word_policy":"unicode_split_whitespace_v1","realized_share":share,
                    "requested_percent_not_target":percent,"full_rewrite_is_mixed_history":percent==100});
                row["generation"] = json!({"generator":model,"binding":binding,"request":request,"response_path":path,"position":position_name,
                    "response_sha256":sha(&fs::read(&path)?),"mode":mode,"attempt":attempt,
                    "response_format_policy":"strip_verified_empty_leading_think_block_then_trim_v1"});
                if style_diversity {
                    row["generation"]["instruction_style"] = json!(style);
                    row["generation"]["style_compliance"] = json!("not_evaluated");
                    row["generation"]["meaning_preservation"] = json!("not_evaluated");
                    row["metadata"]["acquisition_batch"] = binding["recipe"].clone();
                }
                if done.exists() {
                    ensure!(read(&done)? == row, "interrupted completed record differs");
                } else {
                    put(&done, &row)?;
                }
                put(
                    &dir.join("record.sha256.json"),
                    &json!({"sha256":sha(&fs::read(&done)?),"plan_sha256":sha(&fs::read(dir.join("plan.json"))?)}),
                )?;
                result = Some(row);
                break;
            }
            match result { Some(r) => accepted.push(r), None => failures.push(json!({"id":generation_id,"parent_id":id,"mode":mode,"requested_span_percent":percent,"reason":"no_accepted_response_after_bounded_attempts"})) }
            eprintln!(
                "{}: {} accepted, {} failed",
                generation_id,
                accepted.len(),
                failures.len()
            );
        }
    }
    let mut bytes = Vec::new();
    for row in &accepted {
        serde_json::to_writer(&mut bytes, row)?;
        bytes.push(b'\n');
    }
    let final_path = out.join("records.jsonl");
    if final_path.exists() {
        ensure!(fs::read(&final_path)? == bytes, "completed output differs");
    } else {
        let mut f = tempfile::NamedTempFile::new_in(out)?;
        f.write_all(&bytes)?;
        f.as_file().sync_all()?;
        f.persist_noclobber(&final_path)?;
        fs::File::open(out)?.sync_all()?;
    }
    let report = json!({"schema":"slop_ninja_controlled_generation_report_v2","input_sha256":sha(&input),"binding_sha256":sha(&serde_json::to_vec(&binding)?),
        "records_sha256":sha(&bytes),"accepted":accepted.len(),"failures":failures,"complete":true,"test_opened":test_mode});
    if !out.join("report.json").exists() {
        put(&out.join("report.json"), &report)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn expansion_recipe_cannot_be_combined_with_test_mode() {
        let args = |tail: &[&str]| {
            ["input", "output", "config"]
                .into_iter()
                .chain(tail.iter().copied())
                .map(str::to_owned)
                .collect::<Vec<_>>()
        };
        assert_eq!(generation_mode_args(&args(&[])).unwrap(), (false, false));
        assert_eq!(
            generation_mode_args(&args(&["--test"])).unwrap(),
            (true, false)
        );
        assert_eq!(
            generation_mode_args(&args(&["--recipe", "train_scale_v1"])).unwrap(),
            (false, true)
        );
        assert!(generation_mode_args(&args(&["--test", "--recipe", "train_scale_v1"])).is_err());
        assert!(generation_mode_args(&args(&["--recipe", "anything_else"])).is_err());
        assert!(generation_mode_args(&args(&["--recipe", "encoder_stage3_test_v1"])).is_err());
        assert_eq!(
            generation_mode_args(&args(&["--recipe", "encoder_stage3_test_v1", "seal.json"]))
                .unwrap(),
            (true, false)
        );
        assert_eq!(
            generation_mode_args(&args(&["--recipe", "encoder_stage3_v1"])).unwrap(),
            (false, false)
        );
        assert!(generation_mode_args(&args(&["--test", "--recipe", "encoder_stage3_v1"])).is_err());
    }
    #[test]
    fn expansion_scratch_prompt_uses_register_without_source_prose_or_identity() {
        let mut row = json!({"id":"synthetic_private_routing_id", "text":"Synthetic source prose that must not be copied.",
            "human_identity":"synthetic_author", "metadata":{"source_collection":"plos_one"}});
        let p = scale_scratch_prompt(&row).unwrap();
        assert!(p.contains("scientific study"));
        for key in ["id", "text", "human_identity"] {
            assert!(!p.contains(row[key].as_str().unwrap()));
        }
        row["metadata"]["source_collection"] = json!("wikinews");
        assert!(scale_scratch_prompt(&row).unwrap().contains("news report"));
        row["metadata"]["source_collection"] = json!("unreviewed");
        assert!(scale_scratch_prompt(&row).is_err());
    }
    #[test]
    fn realized_share_uses_final_word_count() {
        let (t, s, e, v) = assembled(
            "One two. Three four five.",
            0,
            8,
            "A longer replacement with words.",
        )
        .unwrap();
        assert_eq!(&t[s..e], "A longer replacement with words.");
        assert_eq!(v, 5. / 8.);
    }
    #[test]
    fn unicode_and_sham_are_preserved() {
        let t = "Café 😊 waits. Another sentence remains.";
        let (s, e) = span(t, 25, 0);
        assert_eq!(assembled(t, s, e, &t[s..e]).unwrap().0, t);
        assert!(t.is_char_boundary(e));
    }
    #[test]
    fn no_silent_token_join() {
        assert!(assembled("one two", 1, 3, "x").is_err());
    }
    #[test]
    fn complete_rewrite_is_unit_share() {
        let (t, _, _, v) = assembled("Old words.", 0, 10, "New words live.").unwrap();
        assert_eq!(v, 1.);
        assert_eq!(t, "New words live.");
    }
    #[test]
    fn empty_reasoning_wrapper_only() {
        assert_eq!(
            prose("<think>\n\n</think>\n\nA scene.").unwrap(),
            "A scene."
        );
        assert!(prose("<think>analysis</think>A scene.").is_err());
    }
    #[test]
    fn positions_preserve_exact_splices() {
        let text =
            "Alpha one two. Beta three four. Gamma five six. Delta seven eight. Epsilon nine ten.";
        for p in [10, 20, 40, 60, 80] {
            for position in 0..3 {
                let (s, e) = span(text, p, position);
                assert!(s < e);
                assert_eq!(assembled(text, s, e, &text[s..e]).unwrap().0, text);
                if position == 0 {
                    assert_eq!(s, 0);
                }
                if position == 2 {
                    assert_eq!(e, text.len());
                }
            }
        }
        let (s, e) = span(text, 20, 1);
        assert!(s > 0 && e < text.len());
    }
    #[test]
    fn full_rewrite_and_single_sentence_boundaries() {
        let t = "Café 😊 waits patiently for another morning.";
        assert_eq!(span(t, 100, 1), (0, t.len()));
        for position in 0..3 {
            let (s, e) = span(t, 20, position);
            assert!(s > 0 || e < t.len());
            assert_eq!(assembled(t, s, e, &t[s..e]).unwrap().0, t);
        }
    }
}
