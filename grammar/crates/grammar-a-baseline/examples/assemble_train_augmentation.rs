//! Assemble a pinned Train augmentation run from complete durable generation evidence.
//! assemble_train_augmentation ASSEMBLY_PLAN.json NEW_OUT
#![recursion_limit = "256"]
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
};
#[path = "assemble_train_augmentation/retained_admission.rs"]
mod retained_admission;
#[allow(dead_code)]
#[path = "prepare_supervised_forest/supervision.rs"]
mod supervision;
const PARTS: [&str; 3] = ["train", "development", "calibration"];
const PERCENTAGES: [u64; 5] = [0, 10, 40, 80, 100];
fn words(s: &str) -> usize {
    s.split_whitespace().count()
}
fn sha(b: &[u8]) -> String {
    hex::encode(Sha256::digest(b))
}
fn compact_sha(v: &Value) -> Result<String> {
    Ok(sha(&serde_json::to_vec(v)?))
}
fn str_at<'a>(v: &'a Value, k: &str) -> Result<&'a str> {
    v[k].as_str()
        .filter(|s| !s.is_empty())
        .with_context(|| format!("missing {k}"))
}
fn counter(v: &Value, k: &str) -> Result<u64> {
    v[k].as_u64().with_context(|| format!("counter {k}"))
}
fn number(v: &Value) -> Result<f64> {
    let n = v.as_f64().context("number")?;
    ensure!(n.is_finite() && n >= 0., "invalid number");
    Ok(n)
}
fn digest_at<'a>(v: &'a Value, k: &str) -> Result<&'a str> {
    let s = str_at(v, k)?;
    ensure!(
        s.len() == 64
            && s.bytes()
                .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c)),
        "invalid digest {k}"
    );
    Ok(s)
}
fn array<'a>(v: &'a Value, k: &str) -> Result<&'a Vec<Value>> {
    v[k].as_array().with_context(|| format!("array {k}"))
}
fn ids(v: &Value, k: &str) -> Result<BTreeSet<String>> {
    let mut out = BTreeSet::new();
    for id in array(v, k)? {
        ensure!(
            out.insert(id.as_str().context("ID")?.to_owned()),
            "duplicate {k}"
        );
    }
    Ok(out)
}
fn bytes(p: &Path) -> Result<Vec<u8>> {
    let mut b = Vec::new();
    File::open(p)
        .with_context(|| format!("open {}", p.display()))?
        .take(256 * 1024 * 1024 + 1)
        .read_to_end(&mut b)?;
    ensure!(b.len() <= 256 * 1024 * 1024, "input too large");
    Ok(b)
}
fn load(p: &Path, inputs: &mut Vec<Value>) -> Result<(Value, Vec<u8>)> {
    let b = bytes(p)?;
    let v = serde_json::from_slice(&b)?;
    inputs.push(json!({"path":p,"sha256":sha(&b)}));
    Ok((v, b))
}
fn records(b: &[u8]) -> Result<Vec<Value>> {
    std::str::from_utf8(b)?
        .lines()
        .map(|l| Ok(serde_json::from_str(l)?))
        .collect()
}
fn unique_rows(rows: &[Value]) -> Result<BTreeMap<String, &Value>> {
    let mut map = BTreeMap::new();
    for r in rows {
        ensure!(
            map.insert(str_at(r, "id")?.to_owned(), r).is_none(),
            "duplicate record ID"
        );
    }
    Ok(map)
}
fn write_new(p: &Path, b: &[u8]) -> Result<()> {
    let mut f = OpenOptions::new().create_new(true).write(true).open(p)?;
    f.write_all(b)?;
    f.sync_all()?;
    Ok(())
}
fn span(text: &str, percent: usize, position: usize) -> (usize, usize) {
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
fn task_id(parent: &str, model: &str, percent: u64) -> String {
    let mode = if percent == 0 {
        "from_scratch"
    } else {
        "controlled_rewrite"
    };
    format!(
        "local-{}",
        &sha(format!("{parent}:{model}:{mode}:{percent}:span-v3-train-scale").as_bytes())[..24]
    )
}
fn position(parent: &str, percent: u64) -> &'static str {
    ["start", "middle", "end"]
        [Sha256::digest(format!("{parent}:{percent}").as_bytes())[0] as usize % 3]
}
fn check_plan(plan: &Value, parent: &Value, binding: &Value, percent: u64, id: &str) -> Result<()> {
    ensure!(
        plan["id"] == id
            && plan["parent_id"] == parent["id"]
            && plan["source_sha256"] == parent["text_sha256"]
            && plan["source_record_sha256"] == compact_sha(parent)?
            && plan["binding_sha256"] == compact_sha(binding)?,
        "plan source/binding/hash mismatch"
    );
    ensure!(
        plan["mode"]
            == if percent == 0 {
                "from_scratch"
            } else {
                "controlled_rewrite"
            }
            && plan["requested_source_span_percent"] == percent
            && plan["position"] == position(str_at(parent, "id")?, percent)
            && plan["sham_reassembly_exact"] == true,
        "plan task mismatch"
    );
    let bounds = array(plan, "source_byte_span")?;
    ensure!(bounds.len() == 2, "plan span");
    let s = bounds[0].as_u64().context("start")? as usize;
    let e = bounds[1].as_u64().context("end")? as usize;
    let t = str_at(parent, "text")?;
    ensure!(
        s <= e && e <= t.len() && t.is_char_boundary(s) && t.is_char_boundary(e),
        "plan byte bounds"
    );
    ensure!(
        match percent {
            0 => s == 0 && e == 0,
            100 => s == 0 && e == t.len(),
            10 | 40 | 80 => s < e && (s > 0 || e < t.len()),
            _ => false,
        },
        "plan requested span"
    );
    let req = &plan["request"];
    ensure!(
        req["model"] == binding["generator_config"]["alias"]
            && req["seed"] == 1729
            && req["temperature"] == 0.7
            && req["top_p"] == 0.9
            && req["max_tokens"] == 1024
            && req["stream"] == false
            && req["chat_template_kwargs"]["enable_thinking"] == false
            && req["reasoning_format"] == "none",
        "request settings"
    );
    let messages = array(req, "messages")?;
    ensure!(
        messages.len() == 1 && messages[0]["role"] == "user",
        "one user turn only"
    );
    let prompt = str_at(&messages[0], "content")?;
    let pos = position(str_at(parent, "id")?, percent);
    let expected_span = if percent == 0 {
        (0, 0)
    } else {
        span(
            t,
            percent as usize,
            ["start", "middle", "end"]
                .iter()
                .position(|p| *p == pos)
                .context("position")?,
        )
    };
    ensure!(
        (s, e) == expected_span,
        "span differs from deterministic recipe"
    );
    if percent == 0 {
        ensure!(
            prompt == scale_scratch_prompt(parent)?,
            "scratch prompt differs"
        );
    } else {
        ensure!(
            prompt
                == format!(
                    "Rewrite the following English passage in your own words while preserving its meaning, facts, perspective, register, and approximate length. Return only the replacement prose, retaining paragraph breaks where useful: no title, preface, analysis, quotation delimiters, or explanation.\n\n{}",
                    &t[s..e]
                ),
            "rewrite prompt/source mismatch"
        );
    }
    Ok(())
}
fn validate_descendant(
    r: &Value,
    parent: &Value,
    config: &Value,
    binding: &Value,
    plan: &Value,
    percent: u64,
) -> Result<()> {
    check_config(config)?;
    ensure!(r["frozen_split"] == "train", "non-Train descendant");
    ensure!(
        parent["purpose"] == "query" && r["purpose"] == "query",
        "reference generation forbidden"
    );
    for k in [
        "schema",
        "metadata",
        "source_group",
        "frozen_split",
        "source",
        "rights",
        "exposed_to_grammar",
        "grammar_exposure",
        "documented_human_only",
    ] {
        ensure!(r[k] == parent[k], "descendant changed inherited {k}");
    }
    ensure!(
        r["parent_id"] == parent["id"]
            && r["id"] == plan["id"]
            && r["ai_identity"] == config["identity"]
            && r["ai_role_present"] == true,
        "descendant lineage/AI role"
    );
    ensure!(
        r["human_role_present"] == (percent != 0)
            && if percent == 0 {
                r["human_identity"].is_null()
            } else {
                r["human_identity"] == parent["human_identity"]
            },
        "descendant human role/identity"
    );
    ensure!(
        r["origin"] == if percent == 0 { "model_only" } else { "mixed" },
        "descendant origin"
    );
    ensure!(
        r["evidence"]
            == if percent == 0 {
                "recorded_local_generation_from_scratch"
            } else {
                "recorded_local_rewrite_of_historical_human_proxy"
            },
        "descendant workflow evidence"
    );
    let g = &r["generation"];
    ensure!(
        g["generator"] == config["identity"]
            && g["binding"] == *binding
            && g["request"] == plan["request"]
            && g["position"] == plan["position"]
            && g["mode"] == plan["mode"],
        "generation/plan binding"
    );
    let share = &r["share_evidence"];
    ensure!(
        share["schema"] == "slop_ninja_realized_span_share_v1"
            && share["source_text"] == parent["text"]
            && share["source_sha256"] == parent["text_sha256"]
            && share["source_byte_span"] == plan["source_byte_span"]
            && share["requested_percent_not_target"] == percent
            && share["full_rewrite_is_mixed_history"] == (percent == 100),
        "share/parent/plan binding"
    );
    ensure!(
        r["text_sha256"] == sha(str_at(r, "text")?.as_bytes()) && r["share_target"].is_number(),
        "final text/share missing"
    );
    supervision::Supervision::parse(r)?;
    Ok(())
}
fn exact_outcomes(
    expected: &BTreeSet<String>,
    accepted: &BTreeSet<String>,
    failed: &BTreeSet<String>,
) -> Result<()> {
    ensure!(accepted.is_disjoint(failed), "accepted/failed overlap");
    ensure!(
        accepted.union(failed).cloned().collect::<BTreeSet<_>>() == *expected,
        "incomplete/unexpected generation tasks"
    );
    Ok(())
}
fn prose(s: &str) -> Result<&str> {
    let s = s.trim();
    if let Some(rest) = s.strip_prefix("<think>") {
        let (thinking, body) = rest.split_once("</think>").context("thinking wrapper")?;
        ensure!(thinking.trim().is_empty(), "nonempty thinking");
        Ok(body.trim())
    } else {
        Ok(s)
    }
}
fn generator(
    dir: &Path,
    spec: &Value,
    producer_sha: &str,
    config: &Value,
    config_sha: &str,
    humans: &[Value],
    human_sha: &str,
    inputs: &mut Vec<Value>,
    prompts: &mut BTreeMap<(String, u64), Value>,
) -> Result<(Vec<Value>, Value)> {
    let model = str_at(config, "identity")?;
    // A report is published only at the end; absence/incomplete status aborts.
    let (report, _) = load(&dir.join("report.json"), inputs)?;
    ensure!(
        report["schema"] == "slop_ninja_controlled_generation_report_v2"
            && report["complete"] == true
            && report["test_opened"] == false,
        "generator incomplete/Test report"
    );
    let (binding, _) = load(&dir.join("binding.json"), inputs)?;
    ensure!(
        binding["schema"] == "slop_ninja_controlled_generation_v3"
            && binding["recipe"] == "train_scale_v1"
            && binding["scratch_policy"]
                == "collection_register_and_independent_digest_selected_semantic_topic_v1"
            && binding["rewrite_policy"] == "preserve_register_meaning_facts_perspective_length_v1"
            && binding["input_sha256"] == human_sha
            && binding["generator"] == model
            && binding["generator_config"] == *config
            && binding["generator_config_sha256"] == config_sha
            && binding["test_opened"] == false,
        "generator input/config binding"
    );
    ensure!(
        binding["producer_sha256"] == producer_sha
            && binding["share_word_policy"] == "unicode_split_whitespace_v1"
            && binding["requested_span_percentages"] == json!([10, 40, 80, 100])
            && binding["span_policy"] == "nearest_sentence_start_middle_end_by_source_digest_v2",
        "generation contract changed"
    );
    ensure!(
        binding["seed"] == 1729
            && binding["temperature"] == 0.7
            && binding["top_p"] == 0.9
            && binding["max_tokens"] == 1024
            && binding["thinking"] == false
            && binding["input_schema"] == "slop_ninja_a_supervised_record_v1",
        "binding sampler/input schema"
    );
    ensure!(
        report["input_sha256"] == human_sha && report["binding_sha256"] == compact_sha(&binding)?,
        "report binding"
    );
    let b = bytes(&dir.join("records.jsonl"))?;
    ensure!(
        report["records_sha256"] == sha(&b),
        "generator records hash"
    );
    inputs.push(json!({"path":dir.join("records.jsonl"),"sha256":sha(&b)}));
    let generated = records(&b)?;
    let accepted = unique_rows(&generated)?;
    ensure!(report["accepted"] == generated.len(), "accepted count");
    let mut expected = BTreeMap::new();
    for parent in humans
        .iter()
        .filter(|r| r["purpose"] == "query" && r["frozen_split"] == "train")
    {
        for percent in PERCENTAGES {
            let id = task_id(str_at(parent, "id")?, model, percent);
            ensure!(
                expected.insert(id, (parent, percent)).is_none(),
                "task ID collision"
            );
        }
    }
    ensure!(
        expected.len() == humans.len() * PERCENTAGES.len(),
        "requested generation denominator"
    );
    let failures = array(&report, "failures")?;
    let failed = unique_rows(failures)?;
    exact_outcomes(
        &expected.keys().cloned().collect(),
        &accepted.keys().cloned().collect(),
        &failed.keys().cloned().collect(),
    )?;
    for (id, (parent, percent)) in &expected {
        let task = dir.join(id);
        let (plan, plan_bytes) = load(&task.join("plan.json"), inputs)?;
        check_plan(&plan, parent, &binding, *percent, id)?;
        let key = (str_at(parent, "id")?.to_owned(), *percent);
        let mut semantic = plan["request"].clone();
        semantic.as_object_mut().context("request")?.remove("model");
        if let Some(previous) = prompts.insert(key, semantic.clone()) {
            ensure!(
                previous == semantic,
                "generators received different semantic requests"
            );
        }
        let (context, _) = load(&task.join("context.json"), inputs)?;
        let tokens = counter(&context, "prompt_tokens")?;
        ensure!(
            context["request_sha256"] == compact_sha(&plan["request"])?
                && context["max_output_tokens"] == 1024
                && context["context_capacity"] == 8192
                && context["fits"] == (tokens.checked_add(1024).context("token overflow")? < 8192),
            "context receipt mismatch"
        );
        let mut attempts = BTreeMap::new();
        let mut intents = 0;
        let mut uncertain = Vec::new();
        for n in 0..2 {
            let intent_path = task.join(format!("attempt-{n}.intent.json"));
            let response_path = task.join(format!("attempt-{n}.json"));
            if intent_path.exists() {
                let (intent, _) = load(&intent_path, inputs)?;
                ensure!(
                    intent["request"] == plan["request"]
                        && intent["plan_sha256"] == sha(&plan_bytes)
                        && intent["attempt"] == n,
                    "attempt intent mismatch"
                );
                intents += 1;
                if !response_path.exists() {
                    uncertain.push(n);
                }
            } else {
                ensure!(!response_path.exists(), "response without intent");
            }
            if response_path.exists() {
                let (response, raw) = load(&response_path, inputs)?;
                response_replacement(&response)?;
                attempts.insert(n, (response, sha(&raw)));
            }
        }
        for entry in fs::read_dir(&task)? {
            let name = entry?.file_name().to_string_lossy().into_owned();
            ensure!(
                [
                    "plan.json",
                    "context.json",
                    "record.json",
                    "record.sha256.json",
                    "attempt-0.intent.json",
                    "attempt-1.intent.json",
                    "attempt-0.json",
                    "attempt-1.json"
                ]
                .contains(&name.as_str()),
                "unexpected task evidence file"
            );
        }
        inputs.push(json!({"task_id":id,"intent_count":intents,"response_count":attempts.len(),"uncertain_attempt_indices":uncertain}));
        if let Some(row) = accepted.get(id) {
            ensure!(context["fits"] == true, "accepted unsupported context");
            validate_descendant(row, parent, config, &binding, &plan, *percent)?;
            let (saved, raw) = load(&task.join("record.json"), inputs)?;
            let (evidence, _) = load(&task.join("record.sha256.json"), inputs)?;
            ensure!(
                saved == **row
                    && evidence["sha256"] == sha(&raw)
                    && evidence["plan_sha256"] == sha(&plan_bytes),
                "saved record/plan hash mismatch"
            );
            let n = counter(&row["generation"], "attempt")?;
            ensure!(n < 2 && intents == n + 1, "attempt sequence changed");
            for (_, (prior, _)) in attempts.range(..n) {
                ensure!(
                    response_replacement(prior)?.is_none(),
                    "accepted response skipped before chosen attempt"
                );
            }
            let (response, response_sha) = attempts.get(&n).context("accepted response absent")?;
            let response_path = task.join(format!("attempt-{n}.json")).canonicalize()?;
            ensure!(
                Path::new(str_at(&row["generation"], "response_path")?).canonicalize()?
                    == response_path
                    && row["generation"]["response_sha256"] == *response_sha,
                "response path/hash mismatch"
            );
            ensure!(
                response["status"] == 200
                    && serde_json::from_str::<Value>(str_at(response, "raw_body")?)?
                        == response["body"]
                    && response["body"]["choices"][0]["finish_reason"] == "stop",
                "response not accepted stop"
            );
            let replacement = prose(str_at(
                &response["body"]["choices"][0]["message"],
                "content",
            )?)?;
            ensure!(
                replacement.split_whitespace().count() >= 8
                    && !replacement.contains("<think>")
                    && row["share_evidence"]["replacement"] == replacement,
                "response/replacement mismatch"
            );
        } else {
            for (response, _) in attempts.values() {
                ensure!(
                    response_replacement(response)?.is_none(),
                    "terminal failure contains an acceptable response"
                );
            }
            let failure = failed[id];
            ensure!(
                failure["parent_id"] == parent["id"],
                "failed parent mismatch"
            );
            match str_at(failure, "reason")? {
                "unsupported_context" => ensure!(
                    context["fits"] == false && failure["context"] == context && intents == 0,
                    "context failure receipt"
                ),
                "no_accepted_response_after_bounded_attempts" => ensure!(
                    context["fits"] == true
                        && intents == 2
                        && failure["mode"] == plan["mode"]
                        && failure["requested_span_percent"] == *percent,
                    "bounded failure receipt"
                ),
                _ => anyhow::bail!("unsupported failure reason"),
            }
            ensure!(
                !task.join("record.json").exists() && !task.join("record.sha256.json").exists(),
                "failure has accepted record"
            );
        }
    }
    let actual_tasks = fs::read_dir(dir)?
        .map(|entry| entry.map(|entry| entry.file_name().to_string_lossy().into_owned()))
        .collect::<std::io::Result<Vec<_>>>()?
        .into_iter()
        .filter(|name| name.starts_with("local-"))
        .collect::<BTreeSet<_>>();
    ensure!(
        actual_tasks == expected.keys().cloned().collect(),
        "orphan/missing task directories"
    );
    let summary = json!({"slot":spec["slot"],"generator":model,"requested":expected.len(),"accepted":accepted.len(),"failed":failures.len(),"failures":failures,"complete":true,"input_sha256":human_sha,"config_sha256":config_sha,"binding_sha256":compact_sha(&binding)?,"report":report});
    Ok((generated, summary))
}

fn check_config(c: &Value) -> Result<()> {
    let identity = str_at(c, "identity")?;
    ensure!(
        c["schema"] == "slop_ninja_local_generator_v1"
            && c["allowed_splits"] == json!(["train"])
            && c["rights"]["generation_permitted"] == true,
        "Train generator rights/scope"
    );
    ensure!(
        !identity.to_lowercase().contains("mistral") && !identity.to_lowercase().contains("gemma"),
        "withheld model family forbidden"
    );
    ensure!(
        identity.ends_with(digest_at(c, "sha256")?),
        "model identity/weight digest"
    );
    digest_at(c, "runtime_sha256")?;
    str_at(c, "revision")?;
    str_at(c, "repo")?;
    str_at(&c["rights"], "model_license")?;
    Ok(())
}

// Verify raw evidence even for failed requests. Missing response after a durable
// intent stays uncertain; it is never an instruction to resubmit a request.
fn response_replacement(r: &Value) -> Result<Option<&str>> {
    if let Some(raw) = r["raw_body"].as_str() {
        ensure!(
            serde_json::from_str::<Value>(raw).unwrap_or(Value::Null) == r["body"],
            "raw/parsed response differs"
        );
    } else {
        ensure!(
            (r["status"] == 0 && r["error"].is_string()) || r["body_read_error"].is_string(),
            "response lacks raw/error evidence"
        );
        return Ok(None);
    }
    if r["status"] != 200 || r["body"]["choices"][0]["finish_reason"] != "stop" {
        return Ok(None);
    }
    let Some(content) = r["body"]["choices"][0]["message"]["content"].as_str() else {
        return Ok(None);
    };
    let Ok(replacement) = prose(content) else {
        return Ok(None);
    };
    Ok((words(replacement) >= 8 && !replacement.contains("<think>")).then_some(replacement))
}
fn normalized(s: &str) -> String {
    sha(s
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .as_bytes())
}
fn resolve(base: &Path, v: &Value, key: &str) -> Result<PathBuf> {
    Ok(base.join(str_at(v, key)?))
}
fn pinned_json(path: &Path, expected: &str, inputs: &mut Vec<Value>) -> Result<Value> {
    let (v, b) = load(path, inputs)?;
    ensure!(
        sha(&b) == expected,
        "pinned metadata changed: {}",
        path.display()
    );
    Ok(v)
}
fn roots(dir: &Path, plan: &Value, inputs: &mut Vec<Value>) -> Result<(Vec<Value>, Value, String)> {
    let m = pinned_json(
        &dir.join("manifest.json"),
        digest_at(plan, "roots_manifest_sha256")?,
        inputs,
    )?;
    let retained = m["schema"] == retained_admission::SCHEMA;
    if retained {
        retained_admission::manifest(&m, counter(&m, "records")? as usize)?;
    } else {
        ensure!(
            m["schema"] == "slop_ninja_train_augmentation_roots_v1"
                && m["complete"] == true
                && m["shortfall"] == 0
                && m["test_text_opened"] == false
                && array(&m, "test_ids")?.is_empty(),
            "root selection incomplete/non-Train"
        );
    }
    let b = bytes(&dir.join("humans.jsonl"))?;
    let hash = sha(&b);
    ensure!(
        m["records_sha256"] == hash && plan["roots_records_sha256"] == hash,
        "root records pin"
    );
    inputs.push(json!({"path":dir.join("humans.jsonl"),"sha256":hash}));
    let rows = records(&b)?;
    let map = unique_rows(&rows)?;
    ensure!(
        !rows.is_empty()
            && m["records"] == rows.len()
            && (retained || m["requested"] == rows.len())
            && ids(&m, "non_test_ids")? == map.keys().cloned().collect(),
        "root selected roster"
    );
    let selection = pinned_json(
        &dir.join("selection.json"),
        digest_at(&m, "selection_sha256")?,
        inputs,
    )?;
    let audit = bytes(&dir.join("exclusion-audit.jsonl"))?;
    ensure!(
        m["exclusion_audit_sha256"] == sha(&audit),
        "root exclusion audit changed"
    );
    inputs.push(json!({"path":dir.join("exclusion-audit.jsonl"),"sha256":sha(&audit)}));
    let selected = unique_rows_by(array(&selection, "selected")?, "original_id")?;
    ensure!(
        selected.len() == rows.len() && selection["protocol_sha256"] == m["protocol_sha256"],
        "selected root evidence"
    );
    let original_pins = m["input_files"]
        .as_object()
        .context("root source input bindings")?;
    let train_pins = original_pins
        .iter()
        .filter(|(path, _)| {
            Path::new(path)
                .file_name()
                .is_some_and(|n| n == "train.jsonl")
        })
        .map(|(_, v)| digest_at(v, "sha256"))
        .collect::<Result<Vec<_>>>()?;
    ensure!(train_pins.len() == 1, "one original Train input required");
    let mut families = BTreeSet::new();
    for r in &rows {
        validate_root(r, train_pins[0])?;
        ensure!(
            families.insert(str_at(r, "source_group")?),
            "duplicate human root work"
        );
        let old = &r["source"]["record"];
        let e = selected
            .get(str_at(old, "id")?)
            .context("unselected root")?;
        ensure!(
            e["source_group"] == r["source_group"]
                && e["text_sha256"] == r["text_sha256"]
                && e["collection"] == r["metadata"]["source_collection"]
                && e["rank_sha256"] == r["metadata"]["selection_rank_sha256"],
            "selection/root identity mismatch"
        );
    }
    if retained {
        retained_admission::validate(dir, &m, &rows, &selection, &audit, inputs)?;
    }
    Ok((rows, m, hash))
}
fn unique_rows_by<'a>(rows: &'a [Value], key: &str) -> Result<BTreeMap<String, &'a Value>> {
    let mut map = BTreeMap::new();
    for r in rows {
        ensure!(
            map.insert(str_at(r, key)?.into(), r).is_none(),
            "duplicate {key}"
        );
    }
    Ok(map)
}
fn validate_root(r: &Value, train_sha: &str) -> Result<()> {
    let t = str_at(r, "text")?;
    let old = &r["source"]["record"];
    ensure!(
        r["schema"] == "slop_ninja_a_supervised_record_v1"
            && r["frozen_split"] == "train"
            && r["purpose"] == "query"
            && r["origin"] == "human_only"
            && r["evidence"] == "historical_proxy"
            && r["documented_human_only"] == false
            && r["human_identity"].is_null()
            && r["human_role_present"] == true
            && r["ai_identity"].is_null()
            && r["ai_role_present"] == false
            && r["parent_id"].is_null()
            && r["share_target"] == 0.0,
        "root roles/identity/scope"
    );
    ensure!(
        r["text_sha256"] == sha(t.as_bytes())
            && old["text_sha256"] == r["text_sha256"]
            && r["source"]["original_text_sha256"] == r["text_sha256"]
            && r["source"]["original_train_sha256"] == train_sha
            && r["source"]["byte_span"] == json!([0, t.len()]),
        "root exact original text bindings"
    );
    ensure!(
        old["split"] == "train"
            && old["origin"] == "human_only"
            && old["evidence"] == "historical_proxy"
            && old["parent_id"].is_null()
            && old["generation"].is_null()
            && old["source_group"] == r["source_group"]
            && old["rights"] == r["rights"]
            && old["source"]["collection"] == r["metadata"]["source_collection"],
        "root original history changed"
    );
    ensure!(
        [
            "cmu-books-historical",
            "hc3-wiki-historical",
            "plos_one",
            "wikinews"
        ]
        .contains(&str_at(&r["metadata"], "source_collection")?),
        "unknown source collection"
    );
    ensure!((150..=450).contains(&words(t)), "root length contract");
    for key in [
        "commercial_training",
        "model_release",
        "external_evaluation",
        "redistribute_text",
    ] {
        ensure!(r["rights"][key] == true, "root rights {key}");
    }
    for key in ["license", "attribution", "evidence_url"] {
        str_at(&r["rights"], key)?;
    }
    digest_at(&r["rights"], "evidence_sha256")?;
    digest_at(&r["source"], "original_jsonl_line_sha256")?;
    ensure!(
        r["grammar_exposure"] == "unknown" && r["exposed_to_grammar"].is_null(),
        "conservative exposure changed"
    );
    supervision::Supervision::parse(r)?;
    Ok(())
}
#[derive(Default)]
struct Protected {
    ids: BTreeSet<String>,
    families: BTreeSet<String>,
    texts: BTreeSet<String>,
}
fn protected(base: &Path, specs: &[Value], inputs: &mut Vec<Value>) -> Result<Protected> {
    let mut out = Protected::default();
    ensure!(!specs.is_empty(), "protected current populations required");
    for spec in specs {
        let dir = resolve(base, spec, "directory")?;
        let m = pinned_json(
            &dir.join("manifest.json"),
            digest_at(spec, "manifest_sha256")?,
            inputs,
        )?;
        ensure!(
            m["schema"] == "slop_ninja_a_supervised_dataset_v1" && m["test_text_opened"] == false,
            "protected non-Test manifest"
        );
        let allowed = ids(&m, "non_test_ids")?;
        let reserved = ids(&m, "test_ids")?;
        ensure!(allowed.is_disjoint(&reserved), "Test ID overlap");
        let b = bytes(&dir.join("records.jsonl"))?;
        ensure!(
            m["records_sha256"] == sha(&b) && spec["records_sha256"] == sha(&b),
            "protected record digest"
        );
        inputs.push(json!({"path":dir.join("records.jsonl"),"sha256":sha(&b)}));
        let rows = records(&b)?;
        let map = unique_rows(&rows)?;
        ensure!(
            allowed == map.keys().cloned().collect() && spec["records"] == rows.len(),
            "protected roster/count"
        );
        for r in &rows {
            let id = str_at(r, "id")?;
            let group = str_at(r, "source_group")?;
            let split = str_at(r, "frozen_split")?;
            ensure!(
                PARTS.contains(&split) && m["split_assignments"][group] == split,
                "protected split mismatch"
            );
            let t = str_at(r, "text")?;
            ensure!(r["text_sha256"] == sha(t.as_bytes()), "protected text hash");
            ensure!(out.ids.insert(id.into()), "duplicate protected ID");
            out.families.insert(group.into());
            out.texts.insert(normalized(t));
        }
        out.ids.extend(reserved);
    }
    Ok(out)
}
fn exclude_duplicates(
    humans: &[Value],
    generated: Vec<Value>,
    protected: &Protected,
) -> Result<(Vec<Value>, Vec<Value>)> {
    let mut root_texts = BTreeSet::new();
    for r in humans {
        ensure!(
            !protected.ids.contains(str_at(r, "id")?)
                && !protected.families.contains(str_at(r, "source_group")?),
            "root crosses protected population"
        );
        let hash = normalized(str_at(r, "text")?);
        ensure!(
            !protected.texts.contains(&hash) && root_texts.insert(hash),
            "root duplicates protected/selected text"
        );
    }
    let mut groups = BTreeMap::<String, Vec<Value>>::new();
    for r in generated {
        ensure!(
            !protected.ids.contains(str_at(&r, "id")?)
                && !protected.families.contains(str_at(&r, "source_group")?),
            "generated ID/family crosses protected population"
        );
        groups
            .entry(normalized(str_at(&r, "text")?))
            .or_default()
            .push(r);
    }
    let mut rows = humans.to_vec();
    let mut excluded = Vec::new();
    for (hash, rs) in groups {
        let reason = if protected.texts.contains(&hash) {
            Some("normalized_text_matches_old_protected_population")
        } else if root_texts.contains(&hash) {
            Some("normalized_text_matches_human_root")
        } else if rs.len() > 1 {
            Some("duplicate_generated_normalized_text")
        } else {
            None
        };
        for r in rs {
            if let Some(reason) = reason {
                excluded.push(json!({"id":r["id"],"parent_id":r["parent_id"],"text_sha256":r["text_sha256"],"normalized_text_sha256":hash,"reason":reason,"retained_generation_evidence":true}));
            } else {
                rows.push(r);
            }
        }
    }
    Ok((rows, excluded))
}
fn histogram(rows: &[Value], field: &str) -> BTreeMap<String, usize> {
    let mut m = BTreeMap::new();
    for r in rows {
        *m.entry(r[field].as_str().unwrap_or("null").to_owned())
            .or_default() += 1;
    }
    m
}
fn assemble(plan_path: &Path, out: &Path) -> Result<()> {
    ensure!(!out.exists(), "new output directory required");
    let mut inputs = Vec::new();
    let (plan, pb) = load(plan_path, &mut inputs)?;
    ensure!(
        plan["schema"] == "slop_ninja_train_augmentation_assembly_plan_v1"
            && plan["recipe"] == "train_scale_v1",
        "assembly plan schema/recipe"
    );
    let base = plan_path.parent().context("plan parent")?;
    let specs = array(&plan, "generators")?;
    ensure!(!specs.is_empty(), "no pinned generators");
    // Completed aggregate reports are the admission boundary, never live partial files.
    for spec in specs {
        let dir = resolve(base, spec, "run_directory")?;
        let (r, _) = load(&dir.join("report.json"), &mut inputs)?;
        ensure!(
            r["complete"] == true && r["test_opened"] == false,
            "generation not complete"
        );
    }
    let (humans, root_manifest, human_sha) = roots(
        &resolve(base, &plan, "roots_directory")?,
        &plan,
        &mut inputs,
    )?;
    let protected = protected(base, array(&plan, "protected_populations")?, &mut inputs)?;
    let mut all_generated = Vec::new();
    let mut reports = Vec::new();
    let mut prompts = BTreeMap::new();
    let mut identities = BTreeSet::new();
    let mut slots = BTreeSet::new();
    for spec in specs {
        ensure!(
            slots.insert(str_at(spec, "slot")?),
            "duplicate generator slot"
        );
        let config = pinned_json(
            &resolve(base, spec, "config_file")?,
            digest_at(spec, "config_sha256")?,
            &mut inputs,
        )?;
        ensure!(
            config["identity"] == spec["identity"]
                && identities.insert(str_at(&config, "identity")?.to_owned()),
            "duplicate/changed generator identity"
        );
        check_config(&config)?;
        let (rows, report) = generator(
            &resolve(base, spec, "run_directory")?,
            spec,
            digest_at(&plan, "producer_sha256")?,
            &config,
            digest_at(spec, "config_sha256")?,
            &humans,
            &human_sha,
            &mut inputs,
            &mut prompts,
        )?;
        all_generated.extend(rows);
        reports.push(report);
    }
    unique_rows(
        &humans
            .iter()
            .chain(&all_generated)
            .cloned()
            .collect::<Vec<_>>(),
    )?;
    let accepted_before_exclusions = humans.len() + all_generated.len();
    let (mut rows, excluded) = exclude_duplicates(&humans, all_generated, &protected)?;
    rows.sort_by(|a, b| a["id"].as_str().cmp(&b["id"].as_str()));
    let mut assignments = BTreeMap::new();
    for r in &rows {
        ensure!(
            r["frozen_split"] == "train" && r["purpose"] == "query",
            "Train-only query export"
        );
        assignments.insert(str_at(r, "source_group")?.to_owned(), "train");
        supervision::Supervision::parse(r)?;
    }
    let mut output = Vec::new();
    for r in &rows {
        serde_json::to_writer(&mut output, r)?;
        output.push(b'\n');
    }
    let evidence = serde_json::to_vec_pretty(
        &json!({"schema":"slop_ninja_train_augmentation_evidence_index_v1","inputs":inputs}),
    )?;
    let mut audit = json!({"schema":"slop_ninja_train_augmentation_audit_v1","requested_roots":humans.len(),"requested_generation_tasks":humans.len()*PERCENTAGES.len()*specs.len(),
        "accepted_before_exclusions":accepted_before_exclusions,"included":rows.len(),"excluded":excluded,"generators":reports,
        "origins":histogram(&rows,"origin"),"ai_identities":histogram(&rows,"ai_identity"),"unknown_human_identity_rows":rows.iter().filter(|r|r["human_identity"].is_null()).count(),
        "full_rewrite_mixed_share_one_rows":rows.iter().filter(|r|r["origin"]=="mixed"&&r["share_evidence"]["full_rewrite_is_mixed_history"]==true&&r["share_target"]==1.0).count(),
        "protected_populations":plan["protected_populations"],"test_text_opened":false,"evidence_files_modified":false});
    if root_manifest["schema"] == retained_admission::SCHEMA {
        audit["root_admission"] = retained_admission::summary(&root_manifest);
    }
    let ab = serde_json::to_vec_pretty(&audit)?;
    let mut manifest = json!({"schema":"slop_ninja_a_supervised_dataset_v1","cohort":"train_scale_v1","records":rows.len(),"records_sha256":sha(&output),"input_sha256":sha(&output),
        "non_test_ids":rows.iter().map(|r|r["id"].clone()).collect::<Vec<_>>(),"test_ids":[],"split_assignments":assignments,"source_families":assignments.len(),
        "sources":[{"assembly_plan":plan_path,"sha256":sha(&pb)},{"roots_manifest_sha256":plan["roots_manifest_sha256"],"roots_records_sha256":human_sha},{"evidence_index":"evidence-index.json","sha256":sha(&evidence)}],
        "source_manifest_sha256":plan["roots_manifest_sha256"],"generation_reports":reports,"requested_human_records":humans.len(),"requested_generation_records":humans.len()*PERCENTAGES.len()*specs.len(),
        "requested_records":humans.len()*(1+PERCENTAGES.len()*specs.len()),"accepted_before_exclusions":accepted_before_exclusions,"excluded":excluded,"audit_sha256":sha(&ab),
        "complete":true,"test_text_opened":false,"clean_holdout":false,"train_extension_only":true,"reference_policy":"no references in this Train query extension; existing galleries remain frozen",
        "rights_scope":"original historical source rights and ancestry unchanged; pinned generator model/output evidence retained; ShareAlike model-release policy still applies",
        "root_selection_protocol_sha256":root_manifest["protocol_sha256"],"labels_are_not_inputs":true,"implementation_sha256":sha(include_bytes!("assemble_train_augmentation.rs")),
        "supervision_implementation_sha256":sha(include_bytes!("prepare_supervised_forest/supervision.rs"))});
    if root_manifest["schema"] == retained_admission::SCHEMA {
        manifest["root_admission"] = retained_admission::summary(&root_manifest);
        manifest["retained_admission_implementation_sha256"] = json!(sha(include_bytes!(
            "assemble_train_augmentation/retained_admission.rs"
        )));
    }
    let mb = serde_json::to_vec_pretty(&manifest)?;
    supervision::Dataset::parse(&mb)?;
    fs::create_dir(out)?;
    write_new(&out.join("records.jsonl"), &output)?;
    write_new(&out.join("evidence-index.json"), &evidence)?;
    write_new(&out.join("audit.json"), &ab)?;
    write_new(&out.join("manifest.json"), &mb)?;
    File::open(out)?.sync_all()?;
    println!(
        "{}",
        json!({"records":rows.len(),"records_sha256":sha(&output),"excluded":excluded.len(),"requested_generation":humans.len()*PERCENTAGES.len()*specs.len()})
    );
    Ok(())
}
fn main() -> Result<()> {
    let a = std::env::args().skip(1).collect::<Vec<_>>();
    if a.first().is_some_and(|s| s == "--verify-roots") {
        ensure!(
            a.len() == 3,
            "assemble_train_augmentation --verify-roots ASSEMBLY_PLAN.json NEW_RECEIPT"
        );
        let receipt = Path::new(&a[2]);
        ensure!(!receipt.exists(), "new receipt required");
        let path = Path::new(&a[1]);
        let mut inputs = Vec::new();
        let (plan, _) = load(path, &mut inputs)?;
        ensure!(
            plan["schema"] == "slop_ninja_train_augmentation_assembly_plan_v1"
                && plan["recipe"] == "train_scale_v1",
            "assembly plan schema/recipe"
        );
        let base = path.parent().context("plan parent")?;
        let (rows, m, hash) = roots(
            &resolve(base, &plan, "roots_directory")?,
            &plan,
            &mut inputs,
        )?;
        let p = protected(base, array(&plan, "protected_populations")?, &mut inputs)?;
        let (_, excluded) = exclude_duplicates(&rows, Vec::new(), &p)?;
        ensure!(
            excluded.is_empty(),
            "admitted root conflicts with protected population"
        );
        let result = json!({"schema":"slop_ninja_train_root_verification_v1","verified":true,"records":rows.len(),"records_sha256":hash,
            "root_schema":m["schema"],"root_admission":if m["schema"]==retained_admission::SCHEMA {retained_admission::summary(&m)} else {Value::Null},
            "input_files":inputs,"assembly_performed":false,"test_text_opened":false,"model_calls":0});
        write_new(receipt, &serde_json::to_vec_pretty(&result)?)?;
        println!(
            "{}",
            json!({"verified":true,"records":rows.len(),"records_sha256":hash,"assembly_performed":false})
        );
        return Ok(());
    }
    ensure!(
        a.len() == 2,
        "assemble_train_augmentation ASSEMBLY_PLAN.json NEW_OUT"
    );
    assemble(Path::new(&a[0]), Path::new(&a[1]))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn parent() -> Value {
        let t = (0..160)
            .map(|i| format!("synthetic{i}"))
            .collect::<Vec<_>>()
            .join(" ");
        json!({"schema":"slop_ninja_a_supervised_record_v1","id":"synthetic-root","text":t,"text_sha256":sha(t.as_bytes()),"source_group":"synthetic-work","frozen_split":"train","purpose":"query",
            "origin":"human_only","evidence":"historical_proxy","documented_human_only":false,"human_identity":null,"human_role_present":true,"ai_identity":null,"ai_role_present":false,"share_target":0.,"share_evidence":{"basis":"historical_proxy_endpoint"},
            "source":{"history":"synthetic fixture"},"rights":{"commercial_training":true},"exposed_to_grammar":null,"grammar_exposure":"unknown","metadata":{"source_collection":"plos_one"},"parent_id":null})
    }
    fn config() -> Value {
        json!({"schema":"slop_ninja_local_generator_v1","identity":format!("synthetic/Phi:{}","a".repeat(64)),"sha256":"a".repeat(64),"runtime_sha256":"b".repeat(64),"repo":"synthetic/quantizer","revision":"pinned","allowed_splits":["train"],"alias":"synthetic","rights":{"generation_permitted":true,"model_license":"MIT"}})
    }
    fn binding() -> Value {
        let c = config();
        json!({"schema":"slop_ninja_controlled_generation_v3","recipe":"train_scale_v1","input_sha256":"c".repeat(64),"generator":c["identity"],"generator_config":c,"generator_config_sha256":"d".repeat(64),"producer_sha256":"e".repeat(64),"test_opened":false,
        "scratch_policy":"collection_register_and_independent_digest_selected_semantic_topic_v1","rewrite_policy":"preserve_register_meaning_facts_perspective_length_v1","share_word_policy":"unicode_split_whitespace_v1","span_policy":"nearest_sentence_start_middle_end_by_source_digest_v2","requested_span_percentages":[10,40,80,100],"seed":1729,"temperature":0.7,"top_p":0.9,"max_tokens":1024,"thinking":false,"input_schema":"slop_ninja_a_supervised_record_v1"})
    }
    fn plan(p: &Value, b: &Value, percent: u64) -> Value {
        let id = str_at(p, "id").unwrap();
        let t = str_at(p, "text").unwrap();
        let pos = position(id, percent);
        let (s, e) = if percent == 0 {
            (0, 0)
        } else {
            span(
                t,
                percent as usize,
                ["start", "middle", "end"]
                    .iter()
                    .position(|x| *x == pos)
                    .unwrap(),
            )
        };
        let prompt = if percent == 0 {
            scale_scratch_prompt(p).unwrap()
        } else {
            format!(
                "Rewrite the following English passage in your own words while preserving its meaning, facts, perspective, register, and approximate length. Return only the replacement prose, retaining paragraph breaks where useful: no title, preface, analysis, quotation delimiters, or explanation.\n\n{}",
                &t[s..e]
            )
        };
        json!({"id":task_id(id,str_at(b,"generator").unwrap(),percent),"parent_id":id,"source_sha256":p["text_sha256"],"source_record_sha256":compact_sha(p).unwrap(),"binding_sha256":compact_sha(b).unwrap(),"mode":if percent==0{"from_scratch"}else{"controlled_rewrite"},"requested_source_span_percent":percent,"position":pos,"source_byte_span":[s,e],"sham_reassembly_exact":true,
            "request":{"model":b["generator_config"]["alias"],"messages":[{"role":"user","content":prompt}],"seed":1729,"temperature":0.7,"top_p":0.9,"max_tokens":1024,"stream":false,"chat_template_kwargs":{"enable_thinking":false},"reasoning_format":"none"}})
    }
    fn response(content: &str) -> Value {
        let b = json!({"choices":[{"finish_reason":"stop","message":{"content":content}}]});
        json!({"status":200,"raw_body":serde_json::to_string(&b).unwrap(),"body":b})
    }
    fn write_json(path: &Path, v: &Value) {
        fs::write(path, serde_json::to_vec(v).unwrap()).unwrap();
    }
    fn terminal_fixture(dir: &Path) -> (Value, Value) {
        let p = parent();
        let b = binding();
        let mut failures = Vec::new();
        write_json(&dir.join("binding.json"), &b);
        fs::write(dir.join("records.jsonl"), b"").unwrap();
        for percent in PERCENTAGES {
            let plan = plan(&p, &b, percent);
            let id = str_at(&plan, "id").unwrap();
            let task = dir.join(id);
            fs::create_dir(&task).unwrap();
            write_json(&task.join("plan.json"), &plan);
            write_json(
                &task.join("context.json"),
                &json!({"request_sha256":compact_sha(&plan["request"]).unwrap(),"prompt_tokens":10,"max_output_tokens":1024,"context_capacity":8192,"fits":true}),
            );
            for n in 0..2 {
                write_json(
                    &task.join(format!("attempt-{n}.intent.json")),
                    &json!({"request":plan["request"],"plan_sha256":sha(&fs::read(task.join("plan.json")).unwrap()),"attempt":n}),
                );
                write_json(
                    &task.join(format!("attempt-{n}.json")),
                    &json!({"status":0,"error":"synthetic transport failure"}),
                );
            }
            failures.push(json!({"id":id,"parent_id":p["id"],"reason":"no_accepted_response_after_bounded_attempts","mode":plan["mode"],"requested_span_percent":percent}));
        }
        write_json(
            &dir.join("report.json"),
            &json!({"schema":"slop_ninja_controlled_generation_report_v2","complete":true,"test_opened":false,"input_sha256":b["input_sha256"],"binding_sha256":compact_sha(&b).unwrap(),"records_sha256":sha(b""),"accepted":0,"failures":failures}),
        );
        (p, b)
    }
    fn validate_fixture(dir: &Path, p: &Value) -> Result<(Vec<Value>, Value)> {
        generator(
            dir,
            &json!({"slot":"fixture"}),
            &"e".repeat(64),
            &config(),
            &"d".repeat(64),
            &[p.clone()],
            &"c".repeat(64),
            &mut Vec::new(),
            &mut BTreeMap::new(),
        )
    }
    #[test]
    fn all_tasks_accounted_and_no_acceptable_response_hidden_as_failure() {
        let tmp = tempfile::tempdir().unwrap();
        let (p, b) = terminal_fixture(tmp.path());
        let (rows, report) = validate_fixture(tmp.path(), &p).unwrap();
        assert!(rows.is_empty());
        assert_eq!(report["requested"], 5);
        assert_eq!(report["failed"], 5);
        let task = tmp.path().join(task_id(
            str_at(&p, "id").unwrap(),
            str_at(&b, "generator").unwrap(),
            10,
        ));
        write_json(
            &task.join("attempt-0.json"),
            &response("These eight synthetic words provide a complete accepted output."),
        );
        assert!(
            validate_fixture(tmp.path(), &p)
                .unwrap_err()
                .to_string()
                .contains("acceptable response")
        );
    }
    #[test]
    fn missing_intent_rejected_but_durable_uncertainty_retained() {
        let tmp = tempfile::tempdir().unwrap();
        let (p, b) = terminal_fixture(tmp.path());
        let task = tmp.path().join(task_id(
            str_at(&p, "id").unwrap(),
            str_at(&b, "generator").unwrap(),
            40,
        ));
        fs::remove_file(task.join("attempt-0.json")).unwrap();
        assert!(validate_fixture(tmp.path(), &p).is_ok());
        fs::remove_file(task.join("attempt-1.intent.json")).unwrap();
        assert!(
            validate_fixture(tmp.path(), &p)
                .unwrap_err()
                .to_string()
                .contains("response without intent")
        );
    }
    #[test]
    fn full_rewrite_keeps_mixed_history_and_measured_share_one() {
        let p = parent();
        let b = binding();
        let plan = plan(&p, &b, 100);
        let replacement =
            "This fully rewritten synthetic passage retains a mixed production history.";
        let mut r = p.clone();
        r["id"] = plan["id"].clone();
        r["parent_id"] = p["id"].clone();
        r["origin"] = json!("mixed");
        r["evidence"] = json!("recorded_local_rewrite_of_historical_human_proxy");
        r["ai_identity"] = config()["identity"].clone();
        r["ai_role_present"] = json!(true);
        r["text"] = json!(replacement);
        r["text_sha256"] = json!(sha(replacement.as_bytes()));
        r["share_target"] = json!(1.);
        r["generation"] = json!({"generator":config()["identity"],"binding":b,"request":plan["request"],"position":plan["position"],"mode":"controlled_rewrite"});
        r["share_evidence"] = json!({"schema":"slop_ninja_realized_span_share_v1","source_text":p["text"],"source_sha256":p["text_sha256"],"source_byte_span":plan["source_byte_span"],"requested_percent_not_target":100,"full_rewrite_is_mixed_history":true,"replacement":replacement,"final_byte_span":[0,replacement.len()],"final_sha256":r["text_sha256"],"replacement_words":words(replacement),"final_words":words(replacement),"word_policy":"unicode_split_whitespace_v1","realized_share":1.});
        validate_descendant(&r, &p, &config(), &b, &plan, 100).unwrap();
        assert!(r["human_identity"].is_null());
        assert_eq!(r["human_role_present"], true);
        r["share_target"] = json!(0.5);
        assert!(validate_descendant(&r, &p, &config(), &b, &plan, 100).is_err());
    }
    #[test]
    fn v3_plans_bind_percentages_source_spans_and_scratch_genre() {
        let p = parent();
        let b = binding();
        for percent in PERCENTAGES {
            let mut q = plan(&p, &b, percent);
            check_plan(&q, &p, &b, percent, str_at(&q, "id").unwrap()).unwrap();
            q["request"]["messages"][0]["content"] = json!("Unrelated prompt");
            assert!(check_plan(&q, &p, &b, percent, str_at(&q, "id").unwrap()).is_err());
        }
    }
    #[test]
    fn normalized_protected_and_generated_collisions_are_excluded() {
        let p = parent();
        let mut protected = Protected::default();
        protected.texts.insert(normalized("protected old text"));
        let make = |id: &str, t: &str| {
            let mut r = p.clone();
            r["id"] = json!(id);
            r["text"] = json!(t);
            r["text_sha256"] = json!(sha(t.as_bytes()));
            r
        };
        let (rows, excluded) = exclude_duplicates(
            &[p.clone()],
            vec![
                make("a", "protected\nold text"),
                make("b", "same content"),
                make("c", "same\tcontent"),
                make("d", "unique content"),
            ],
            &protected,
        )
        .unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(excluded.len(), 3);
        protected.ids.insert(str_at(&p, "id").unwrap().into());
        assert!(exclude_duplicates(&[p], Vec::new(), &protected).is_err());
    }
    #[test]
    fn accounting_rejects_missing_extra_and_overlapping_outcomes() {
        let set = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<BTreeSet<_>>();
        let expected = set(&["a", "b"]);
        exact_outcomes(&expected, &set(&["a"]), &set(&["b"])).unwrap();
        assert!(exact_outcomes(&expected, &set(&["a"]), &set(&[])).is_err());
        assert!(exact_outcomes(&expected, &set(&["a"]), &set(&["a", "b"])).is_err());
        assert!(exact_outcomes(&expected, &set(&["a", "extra"]), &set(&["b"])).is_err());
    }
    #[test]
    fn scope_and_config_pins_fail_closed() {
        let mut c = config();
        check_config(&c).unwrap();
        c["allowed_splits"] = json!(["train", "test"]);
        assert!(check_config(&c).is_err());
        c = config();
        c["identity"] = json!(format!("Mistral:{}", "a".repeat(64)));
        assert!(check_config(&c).is_err());
        let tmp = tempfile::tempdir().unwrap();
        write_json(&tmp.path().join("pinned.json"), &config());
        assert!(
            pinned_json(
                &tmp.path().join("pinned.json"),
                &"f".repeat(64),
                &mut Vec::new()
            )
            .is_err()
        );
        let plan = tmp.path().join("plan.json");
        write_json(
            &plan,
            &json!({"schema":"slop_ninja_train_augmentation_assembly_plan_v1","recipe":"train_scale_v1","generators":[{"run_directory":"unfinished"}]}),
        );
        let out = tmp.path().join("new");
        assert!(assemble(&plan, &out).is_err());
        assert!(!out.exists());
    }
}
