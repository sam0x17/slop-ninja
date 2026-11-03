//! Attributed model samples through explicit APIs or installed authenticated CLIs.
use crate::util::{digest, read, read_jsonl, write_json};
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

pub fn load_prompts(path: &Path) -> Result<Vec<Value>> {
    let prompts = read_jsonl(path)?;
    ensure!(!prompts.is_empty(), "Prompt manifest is empty");
    let mut ids = BTreeSet::new();
    let mut groups = BTreeMap::new();
    for p in &prompts {
        for k in ["id", "prompt", "domain", "register", "group_id"] {
            ensure!(
                p[k].as_str().is_some_and(|s| !s.trim().is_empty()),
                "Prompt requires {k}"
            );
        }
        let id = p["id"].as_str().unwrap();
        ensure!(ids.insert(id), "Duplicate prompt ID {id}");
        let split = p["split"].as_str().unwrap_or("train");
        ensure!(
            ["train", "dev", "test", "exploratory"].contains(&split),
            "Invalid split"
        );
        let group = p["group_id"].as_str().unwrap();
        if let Some(old) = groups.insert(group, split) {
            ensure!(old == split, "Source family crosses splits");
        }
    }
    Ok(prompts)
}

pub fn parse_cli(
    provider: &str,
    requested: &str,
    events: &[Value],
) -> Result<(String, String, Value)> {
    if provider == "codex-cli" {
        ensure!(
            events.iter().any(|e| e["type"] == "turn.completed"),
            "Codex turn did not complete"
        );
        ensure!(
            !events
                .iter()
                .any(|e| e["type"] == "turn.failed" || e["type"] == "error"),
            "Codex reported a failed turn"
        );
        let mut messages = Vec::new();
        for event in events {
            if event["type"]
                .as_str()
                .is_some_and(|s| s.starts_with("item."))
            {
                let kind = event["item"]["type"].as_str().unwrap_or("");
                ensure!(
                    ["agent_message", "reasoning"].contains(&kind),
                    "Codex used a tool or non-prose item: {kind}"
                );
                if event["type"] == "item.completed" && kind == "agent_message" {
                    messages.push(
                        event["item"]["text"]
                            .as_str()
                            .context("Missing Codex text")?,
                    );
                }
            }
        }
        ensure!(
            messages.len() == 1,
            "Expected one final Codex prose message"
        );
        Ok((
            messages[0].to_string(),
            format!("requested:{requested}"),
            json!({
            "model_identity_source":"explicit_cli_request; resolved_model_not_reported",
            "usage":events.iter().find(|e|e["type"]=="turn.completed").map(|e|&e["usage"]),
            "session_id":events.iter().find(|e|e["type"]=="thread.started").map(|e|&e["thread_id"])}),
        ))
    } else {
        let result = events
            .iter()
            .rev()
            .find(|e| e["type"] == "result")
            .context("No Claude result")?;
        ensure!(
            result["subtype"] == "success" && result["is_error"] == false,
            "Claude did not complete successfully"
        );
        let init = events
            .iter()
            .find(|e| e["type"] == "system" && e["subtype"] == "init")
            .context("No Claude init metadata")?;
        ensure!(
            init["tools"].as_array().is_some_and(|a| a.is_empty()),
            "Claude tools were not disabled"
        );
        ensure!(
            init["mcp_servers"].as_array().is_some_and(|a| a.is_empty()),
            "Claude MCP servers were not disabled"
        );
        let mut models = BTreeSet::new();
        for e in events.iter().filter(|e| e["type"] == "assistant") {
            if let Some(model) = e["message"]["model"].as_str() {
                models.insert(model);
            }
            for block in e["message"]["content"]
                .as_array()
                .context("Missing Claude content")?
            {
                ensure!(
                    block["type"] != "fallback",
                    "Claude switched models; keep this invocation separate from the requested model corpus"
                );
                ensure!(
                    ["text", "thinking", "redacted_thinking"]
                        .contains(&block["type"].as_str().unwrap_or("")),
                    "Claude used a tool/non-prose output"
                );
            }
        }
        ensure!(
            models.len() == 1,
            "Claude must report exactly one resolved model"
        );
        if requested.starts_with("claude-") {
            ensure!(
                models.contains(requested),
                "Claude returned a different model than explicitly requested"
            );
        }
        Ok((
            result["result"]
                .as_str()
                .context("Missing Claude prose")?
                .to_string(),
            models.into_iter().next().unwrap().to_string(),
            json!({
            "model_identity_source":"assistant_message.model","usage":result["usage"],"model_usage":result["modelUsage"],
            "reported_cost_usd":result["total_cost_usd"],"session_id":result["session_id"],"init":init}),
        ))
    }
}

/// Reasoning effort for hosted corpus generation; recorded with every capture.
pub const CLI_REASONING_EFFORT: &str = "medium";

/// Isolated, tool-free CLI invocation recorded with every capture.
pub fn cli_args(provider: &str, model: &str) -> Vec<String> {
    let effort = format!("model_reasoning_effort=\"{CLI_REASONING_EFFORT}\"");
    let args: Vec<&str> = if provider == "codex-cli" {
        vec![
            "exec",
            "--ignore-user-config",
            "--disable",
            "shell_tool",
            "--disable",
            "shell_snapshot",
            "--ephemeral",
            "--skip-git-repo-check",
            "--sandbox",
            "read-only",
            "--model",
            model,
            "-c",
            &effort,
            "--json",
            "-",
        ]
    } else {
        vec![
            "-p",
            "--safe-mode",
            "--tools",
            "",
            "--no-session-persistence",
            "--max-turns",
            "1",
            "--model",
            model,
            "--output-format",
            "stream-json",
            "--verbose",
            "--effort",
            CLI_REASONING_EFFORT,
        ]
    };
    args.into_iter().map(str::to_owned).collect()
}

fn collect_cli(
    prompt: &Value,
    provider: &str,
    model: &str,
    run_dir: &Path,
    timeout: Duration,
) -> Result<Value> {
    fs::create_dir_all(run_dir)?;
    let work = run_dir.join("workdir");
    fs::create_dir_all(&work)?;
    let program = if provider == "codex-cli" {
        "codex"
    } else {
        "claude"
    };
    let version = Command::new(program)
        .arg("--version")
        .output()
        .context("Read CLI version")?;
    ensure!(version.status.success(), "Cannot identify CLI version");
    let args = cli_args(provider, model);
    let stdout = fs::File::create(run_dir.join("stdout.jsonl"))?;
    let stderr = fs::File::create(run_dir.join("stderr.txt"))?;
    let mut child = Command::new(program)
        .args(&args)
        .current_dir(work)
        .stdin(Stdio::piped())
        .stdout(stdout)
        .stderr(stderr)
        .spawn()?;
    let mut input = child.stdin.take().context("Missing CLI stdin")?;
    input.write_all(prompt["prompt"].as_str().unwrap().as_bytes())?;
    drop(input);
    let deadline = Instant::now() + timeout;
    let exit = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if Instant::now() >= deadline {
            child.kill()?;
            child.wait()?;
            bail!("CLI timed out; output preserved and invocation was not retried");
        }
        thread::sleep(Duration::from_millis(100));
    };
    ensure!(
        exit.success(),
        "CLI exited unsuccessfully; inspect captured output in {}",
        run_dir.display()
    );
    let events = read_jsonl(&run_dir.join("stdout.jsonl"))?;
    let (text, reported, metadata) = parse_cli(provider, model, &events)?;
    ensure!(!text.trim().is_empty(), "CLI returned empty prose");
    Ok(json!({"text":text,"model":reported,"metadata":{
        "transport":"authenticated_cli","cli_version":String::from_utf8(version.stdout)?.trim(),
        "argv":args,"reasoning_effort":CLI_REASONING_EFFORT,"events":events,"generation_metadata":metadata,
        "cli_internal_retries":"Controlled by CLI; this harness invokes once and records the event stream"}}))
}

fn collect_api(
    prompt: &Value,
    provider: &str,
    model: &str,
    max_tokens: u64,
    timeout: Duration,
    run_dir: &Path,
) -> Result<Value> {
    let (url, keyname) = match provider {
        "openai" => ("https://api.openai.com/v1/responses", "OPENAI_API_KEY"),
        "anthropic" => ("https://api.anthropic.com/v1/messages", "ANTHROPIC_API_KEY"),
        _ => bail!("Unknown provider"),
    };
    let key = std::env::var(keyname).with_context(|| format!("Set {keyname}"))?;
    ensure!(!key.trim().is_empty(), "Set {keyname}");
    let client = reqwest::blocking::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .retry(reqwest::retry::never())
        .timeout(timeout)
        .build()?;
    let mut req = client.post(url).bearer_auth(&key);
    let payload = if provider == "openai" {
        json!({"model":model,"input":prompt["prompt"],"store":false,"max_output_tokens":max_tokens})
    } else {
        req = req.header("anthropic-version", "2023-06-01");
        if let Ok(id) = std::env::var("ANTHROPIC_WORKSPACE_ID") {
            req = req.header("anthropic-workspace-id", id);
        }
        json!({"model":model,"messages":[{"role":"user","content":prompt["prompt"]}],"max_tokens":max_tokens})
    };
    let response = req
        .json(&payload)
        .send()
        .map_err(|_| anyhow::anyhow!("Provider transport failed; no automatic retry"))?;
    ensure!(
        response.status().is_success(),
        "Provider HTTP {}; not retried",
        response.status()
    );
    let response: Value = response.json().context("Invalid provider JSON")?;
    // Keep the complete billed response, including usage and incomplete/refusal
    // details, even when the following corpus acceptance checks reject it.
    write_json(&run_dir.join("response.json"), &response)?;
    let model = response["model"]
        .as_str()
        .context("Provider omitted resolved model")?;
    ensure!(
        !model.trim().is_empty()
            && response["id"]
                .as_str()
                .is_some_and(|s| !s.trim().is_empty()),
        "Provider must report nonempty model and response ID"
    );
    let mut text = String::new();
    if provider == "openai" {
        ensure!(
            response["status"] == "completed" && response["error"].is_null(),
            "OpenAI generation incomplete or rejected"
        );
        for item in response["output"]
            .as_array()
            .context("Missing OpenAI output")?
        {
            if item["type"] == "reasoning" {
                continue;
            }
            ensure!(
                item["type"] == "message"
                    && item["role"] == "assistant"
                    && item["status"] == "completed",
                "Non-prose OpenAI output"
            );
            for part in item["content"].as_array().context("Missing content")? {
                ensure!(part["type"] == "output_text", "Refusal or non-text output");
                text.push_str(part["text"].as_str().context("Missing text")?);
            }
        }
    } else {
        ensure!(
            response["stop_reason"] == "end_turn"
                && response["role"] == "assistant"
                && response["type"] == "message",
            "Anthropic generation incomplete"
        );
        ensure!(
            response["stop_details"]["type"] != "refusal",
            "Anthropic refused"
        );
        for part in response["content"]
            .as_array()
            .context("Missing Anthropic content")?
        {
            if part["type"] == "thinking" || part["type"] == "redacted_thinking" {
                continue;
            }
            ensure!(part["type"] == "text", "Non-prose Anthropic output");
            text.push_str(part["text"].as_str().context("Missing text")?);
        }
    }
    ensure!(!text.trim().is_empty(), "Empty provider prose");
    Ok(
        json!({"text":text,"model":model,"metadata":{"transport":"api","request_parameters":payload,"response":response,"model_identity_source":"response.model"}}),
    )
}

fn validate_cached(
    record: &Value,
    prompt: &Value,
    provider: &str,
    model: &str,
    corpus: &str,
) -> Result<()> {
    ensure!(
        record["status"] == "accepted",
        "Uncertain/rejected previous invocation; inspect before any resubmission"
    );
    let doc = &record["document"];
    ensure!(
        record["prompt"] == *prompt
            && record["provider"] == provider
            && record["model_requested"] == model,
        "Cached collection request provenance mismatch"
    );
    ensure!(
        doc["corpus"] == corpus
            && doc["source_kind"] == "model"
            && doc["provider"] == provider
            && doc["model_requested"] == model,
        "Cached document model/corpus provenance mismatch"
    );
    for key in ["domain", "register", "group_id"] {
        ensure!(doc[key] == prompt[key], "Cached document {key} mismatch");
    }
    ensure!(
        doc["split"] == prompt.get("split").cloned().unwrap_or(json!("train")),
        "Cached document split mismatch"
    );
    let text = doc["text"]
        .as_str()
        .context("Cached document text missing")?;
    ensure!(
        !text.trim().is_empty() && doc["model"].as_str().is_some_and(|s| !s.trim().is_empty()),
        "Cached text/model empty"
    );
    ensure!(
        doc["metadata"]["text_sha256"] == digest(text)
            && doc["metadata"]["prompt_record"] == *prompt,
        "Cached text hash or prompt mismatch"
    );
    Ok(())
}

pub fn collect_batch(
    prompts_path: &Path,
    provider: &str,
    model: &str,
    corpus: &str,
    out: &Path,
    max_requests: usize,
    max_tokens: u64,
) -> Result<Value> {
    ensure!(
        ["openai", "anthropic", "codex-cli", "claude-cli"].contains(&provider),
        "Unknown provider"
    );
    ensure!(
        !model.trim().is_empty() && !corpus.trim().is_empty() && max_tokens > 0,
        "Model, corpus and output limit required"
    );
    let prompts = load_prompts(prompts_path)?;
    let runs = out.with_extension("runs");
    fs::create_dir_all(&runs)?;
    let mut manifest = json!({"provider":provider,"model":model,"corpus":corpus,"max_output_tokens":max_tokens,"prompts":prompts});
    if provider.ends_with("-cli") {
        // A resumed CLI collection never mixes reasoning-effort settings.
        manifest["reasoning_effort"] = json!(CLI_REASONING_EFFORT);
    }
    let manifest_path = runs.join("manifest.json");
    if manifest_path.exists() {
        ensure!(
            serde_json::from_str::<Value>(&read(&manifest_path)?)? == manifest,
            "Existing collection has different inputs"
        );
    } else {
        ensure!(
            !out.exists(),
            "Output already exists without its collection manifest"
        );
        write_json(&manifest_path, &manifest)?;
    }
    let mut missing = 0;
    for prompt in &prompts {
        let path = runs
            .join(digest(prompt["id"].as_str().unwrap()))
            .join("record.json");
        if path.exists() {
            let r: Value = serde_json::from_str(&read(&path)?)?;
            validate_cached(&r, prompt, provider, model, corpus)?;
        } else {
            missing += 1;
        }
    }
    ensure!(
        missing <= max_requests,
        "Needs {missing} new invocations but budget is {max_requests}"
    );
    let mut docs = Vec::new();
    for prompt in &prompts {
        let dir = runs.join(digest(prompt["id"].as_str().unwrap()));
        let path = dir.join("record.json");
        let record = if path.exists() {
            serde_json::from_str::<Value>(&read(&path)?)?
        } else {
            let mut intent = json!({"status":"submission_uncertain","provider":provider,"model_requested":model,
                "requested_at":chrono::Utc::now().to_rfc3339(),"prompt":prompt});
            write_json(&path, &intent)?;
            let collected = if provider.ends_with("-cli") {
                collect_cli(prompt, provider, model, &dir, Duration::from_secs(300))
            } else {
                collect_api(
                    prompt,
                    provider,
                    model,
                    max_tokens,
                    Duration::from_secs(180),
                    &dir,
                )
            };
            match collected {
                Ok(sample) => {
                    let mut metadata = sample["metadata"].clone();
                    metadata["prompt_record"] = prompt.clone();
                    metadata["model_requested"] = json!(model);
                    metadata["requested_at"] = intent["requested_at"].clone();
                    metadata["received_at"] = json!(chrono::Utc::now().to_rfc3339());
                    metadata["collector_version"] = json!("rust-v1");
                    metadata["text_sha256"] = json!(digest(sample["text"].as_str().unwrap()));
                    let doc = json!({"text":sample["text"],"model":sample["model"],"corpus":corpus,"source_kind":"model","provider":provider,
                        "model_requested":model,"domain":prompt["domain"],"register":prompt["register"],"group_id":prompt["group_id"],
                        "split":prompt.get("split").cloned().unwrap_or(json!("train")),"metadata":metadata});
                    intent["status"] = json!("accepted");
                    intent["document"] = doc;
                    write_json(&path, &intent)?;
                }
                Err(e) => {
                    intent["status"] = json!("failed_or_rejected");
                    intent["error"] = json!(e.to_string());
                    write_json(&path, &intent)?;
                    return Err(e);
                }
            }
            eprintln!(
                "Collected {} via {provider}",
                prompt["id"].as_str().unwrap()
            );
            intent
        };
        docs.push(record["document"].clone());
        let parent = out.parent().unwrap_or(Path::new("."));
        fs::create_dir_all(parent)?;
        let mut temp = tempfile::NamedTempFile::new_in(parent)?;
        for doc in &docs {
            serde_json::to_writer(&mut temp, doc)?;
            temp.write_all(b"\n")?;
        }
        temp.as_file().sync_all()?;
        temp.persist(out)?;
    }
    Ok(
        json!({"documents":docs.len(),"new_invocations":missing,"path":out,"provider":provider,"model_requested":model}),
    )
}

pub fn make_rewrite_prompts(human_path: &Path, out: &Path) -> Result<Value> {
    let docs = read_jsonl(human_path)?;
    ensure!(
        !out.exists(),
        "Prompt file exists; preserve the frozen manifest"
    );
    fs::create_dir_all(out.parent().unwrap_or(Path::new(".")))?;
    let mut f = fs::File::create(out)?;
    for doc in &docs {
        let text = doc["text"].as_str().context("Missing source text")?;
        let n = text.split_whitespace().count();
        let prompt = format!(
            "Rewrite the scientific abstract below in your own natural prose. Preserve every factual claim, numerical value, qualification, method, result, and conclusion. Keep the same scientific register and tonal intention. Aim for {n} words, within roughly 10 percent, without omitting details to meet length. Return only the rewritten abstract, with no heading, commentary, or quotation marks around it. Do not use tools or external information.\n\nSOURCE ABSTRACT:\n{text}"
        );
        let p = json!({"id":format!("rewrite-{}",&digest(doc["group_id"].as_str().context("Missing source group")?)[..16]),"prompt":prompt,
            "domain":doc["domain"],"register":doc["register"],"group_id":doc["group_id"],"split":doc["split"],
            "generation_regime":"source-conditioned-rewrite","source_sha256":digest(text),"source_metadata":doc["metadata"],"length_target_whitespace_words":n});
        serde_json::to_writer(&mut f, &p)?;
        f.write_all(b"\n")?;
    }
    Ok(json!({"prompts":docs.len(),"path":out,"regime":"source-conditioned-rewrite"}))
}

/// Freeze one process prompt per selected model source, excluding source prompt metadata.
pub fn make_edit_prompts(
    source_paths: &[PathBuf],
    instructions: &Path,
    split: &str,
    out: &Path,
) -> Result<Value> {
    ensure!(
        !source_paths.is_empty(),
        "At least one model source file is required"
    );
    ensure!(
        ["train", "dev", "test", "exploratory"].contains(&split),
        "An explicit valid split is required"
    );
    let instructions_text = read(instructions)?;
    ensure!(
        !instructions_text.trim().is_empty(),
        "Explicit nonempty process instructions are required"
    );
    let instructions_sha256 = digest(&instructions_text);
    let mut prompts = BTreeMap::new();
    for source_path in source_paths {
        let input = read(source_path)?;
        let input_sha256 = digest(&input);
        for (index, line) in input.lines().enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            let doc: Value = serde_json::from_str(line)
                .with_context(|| format!("{} record {}", source_path.display(), index + 1))?;
            // Other splits are parsed as JSON but their text and metadata are unused.
            if doc["split"] != split {
                continue;
            }
            ensure!(
                doc["source_kind"] == "model",
                "Edit sources must have model provenance"
            );
            for field in [
                "corpus", "group_id", "text", "provider", "model", "domain", "register",
            ] {
                ensure!(
                    doc[field]
                        .as_str()
                        .is_some_and(|value| !value.trim().is_empty()),
                    "Selected model source requires nonempty {field}"
                );
            }
            let corpus = doc["corpus"].as_str().unwrap();
            let group = doc["group_id"].as_str().unwrap();
            let text = doc["text"].as_str().unwrap();
            let source_sha256 = digest(text);
            let id_material = serde_json::to_string(&json!([corpus, group, source_sha256]))?;
            let id = format!("edit-{}", digest(&id_material));
            let requested = [
                doc.get("model_requested"),
                doc["metadata"].get("model_requested"),
            ]
            .into_iter()
            .flatten()
            .find(|value| value.as_str().is_some_and(|model| !model.trim().is_empty()))
            .cloned()
            .unwrap_or(Value::Null);
            let prompt = json!({
                "id":id,
                "prompt":format!("{instructions_text}\n\nSOURCE TEXT:\n{text}"),
                "domain":doc["domain"],"register":doc["register"],"group_id":group,"split":split,
                "generation_regime":"fixed-process-model-text-edit",
                "source_corpus":corpus,"source_sha256":source_sha256,
                "instructions_sha256":instructions_sha256,
                "metadata":{
                    "source_key":[corpus,group],"source_provider":doc["provider"],
                    "source_model":doc["model"],"source_model_requested":requested,
                    "source_sha256":source_sha256,"source_input_sha256":input_sha256,
                },
            });
            ensure!(
                prompts
                    .insert((corpus.to_string(), group.to_string()), prompt)
                    .is_none(),
                "Duplicate source corpus/group key: {corpus} / {group}"
            );
        }
    }
    ensure!(
        !prompts.is_empty(),
        "No model sources in selected split {split}"
    );
    let prompts: Vec<Value> = prompts.into_values().collect();
    let cached = out.exists();
    if cached {
        ensure!(
            read_jsonl(out)? == prompts,
            "Frozen edit prompts differ from these inputs; refusing to overwrite {}",
            out.display()
        );
    } else {
        let parent = out
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        fs::create_dir_all(parent)?;
        let mut temp = tempfile::NamedTempFile::new_in(parent)?;
        for prompt in &prompts {
            serde_json::to_writer(&mut temp, prompt)?;
            temp.write_all(b"\n")?;
        }
        temp.as_file().sync_all()?;
        // A concurrent creator must not replace an already frozen prompt file.
        temp.persist_noclobber(out)?;
    }
    Ok(json!({
        "prompts":prompts.len(),"path":out,"split":split,"cached":cached,
        "instructions_sha256":instructions_sha256,"regime":"fixed-process-model-text-edit",
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn edit_source(corpus: &str, split: &str) -> Value {
        json!({"text":"  Model prose retains its exact spacing.\n", "corpus":corpus,
            "source_kind":"model", "provider":"fixture", "model":"requested:example-v1",
            "model_requested":"example-v1", "domain":"science", "register":"abstract",
            "group_id":"shared-group", "split":split,
            "metadata":{"model_requested":"example-v1", "prompt_record":{"prompt":"SECRET_ORIGINAL_HUMAN_TEXT"},
                "source_metadata":{"human_text":"SECRET_ORIGINAL_HUMAN_TEXT"},"detector_output":"SECRET_DETECTOR_OUTPUT"}})
    }

    #[test]
    fn edit_prompts_distinguish_same_group_across_corpora_and_freeze_inputs() {
        let directory = tempfile::tempdir().unwrap();
        let sources = directory.path().join("sources.jsonl");
        let instructions = directory.path().join("instructions.txt");
        let out = directory.path().join("prompts.jsonl");
        let mut second_source = edit_source("model-b", "dev");
        second_source["model_requested"] = Value::Null;
        fs::write(
            &sources,
            format!("{}\n{}\n", edit_source("model-a", "dev"), second_source),
        )
        .unwrap();
        fs::write(&instructions, "Preserve all claims.\n").unwrap();
        let result =
            make_edit_prompts(std::slice::from_ref(&sources), &instructions, "dev", &out).unwrap();
        assert_eq!(result["prompts"], 2);
        assert_eq!(result["cached"], false);
        let prompts = load_prompts(&out).unwrap();
        assert_ne!(prompts[0]["id"], prompts[1]["id"]);
        assert_eq!(prompts[0]["group_id"], prompts[1]["group_id"]);
        assert_eq!(
            prompts[0]["prompt"],
            "Preserve all claims.\n\n\nSOURCE TEXT:\n  Model prose retains its exact spacing.\n"
        );
        assert_eq!(
            prompts[0]["source_sha256"],
            digest("  Model prose retains its exact spacing.\n")
        );
        assert_eq!(
            prompts[0]["instructions_sha256"],
            digest("Preserve all claims.\n")
        );
        assert_eq!(
            prompts[0]["metadata"]["source_model_requested"],
            "example-v1"
        );
        assert_eq!(
            prompts[1]["metadata"]["source_model_requested"],
            "example-v1"
        );
        let original_bytes = fs::read(&out).unwrap();
        assert_eq!(
            make_edit_prompts(std::slice::from_ref(&sources), &instructions, "dev", &out).unwrap()
                ["cached"],
            true
        );
        assert_eq!(fs::read(&out).unwrap(), original_bytes);
        fs::write(&instructions, "Different instructions.").unwrap();
        assert!(
            make_edit_prompts(std::slice::from_ref(&sources), &instructions, "dev", &out)
                .unwrap_err()
                .to_string()
                .contains("refusing to overwrite")
        );
        assert_eq!(fs::read(&out).unwrap(), original_bytes);
    }

    #[test]
    fn edit_prompts_filter_splits_reject_humans_and_never_copy_source_prompts() {
        let directory = tempfile::tempdir().unwrap();
        let sources = directory.path().join("sources.jsonl");
        let instructions = directory.path().join("instructions.txt");
        let out = directory.path().join("prompts.jsonl");
        let selected = edit_source("model-a", "dev");
        let heldout = json!({"split":"test","source_kind":"human","text":"SECRET_HELDOUT_TEXT","metadata":"not an object"});
        fs::write(&sources, format!("{selected}\n{heldout}\n")).unwrap();
        fs::write(&instructions, "Preserve all claims.").unwrap();
        make_edit_prompts(std::slice::from_ref(&sources), &instructions, "dev", &out).unwrap();
        let output = read(&out).unwrap();
        for secret in [
            "SECRET_ORIGINAL_HUMAN_TEXT",
            "SECRET_HELDOUT_TEXT",
            "SECRET_DETECTOR_OUTPUT",
            "source_metadata",
        ] {
            assert!(!output.contains(secret));
        }
        assert_eq!(load_prompts(&out).unwrap().len(), 1);
        let mut human = selected.clone();
        human["source_kind"] = json!("human");
        fs::write(&sources, format!("{human}\n")).unwrap();
        let other_out = directory.path().join("other.jsonl");
        assert!(
            make_edit_prompts(
                std::slice::from_ref(&sources),
                &instructions,
                "dev",
                &other_out
            )
            .unwrap_err()
            .to_string()
            .contains("model provenance")
        );
        assert!(!other_out.exists());
        fs::write(&sources, format!("{selected}\n{selected}\n")).unwrap();
        assert!(
            make_edit_prompts(
                std::slice::from_ref(&sources),
                &instructions,
                "dev",
                &other_out
            )
            .unwrap_err()
            .to_string()
            .contains("Duplicate source corpus/group")
        );
        assert!(!other_out.exists());
    }

    #[test]
    fn hosted_generation_requests_medium_reasoning_effort() {
        let codex = cli_args("codex-cli", "gpt-example");
        assert!(codex.contains(&"model_reasoning_effort=\"medium\"".to_owned()));
        let claude = cli_args("claude-cli", "sonnet");
        let at = claude.iter().position(|a| a == "--effort").unwrap();
        assert_eq!(claude[at + 1], "medium");
        for args in [codex, claude] {
            assert!(!args.iter().any(|a| a.contains("low") || a.contains("high")));
        }
    }
    #[test]
    fn codex_identity_is_not_fabricated() {
        let events = vec![
            json!({"type":"item.completed","item":{"type":"agent_message","text":"Some prose."}}),
            json!({"type":"turn.completed","usage":{}}),
        ];
        let (_, model, meta) = parse_cli("codex-cli", "gpt-example", &events).unwrap();
        assert_eq!(model, "requested:gpt-example");
        assert!(
            meta["model_identity_source"]
                .as_str()
                .unwrap()
                .contains("not_reported")
        );
    }
    #[test]
    fn codex_tool_use_is_rejected() {
        let events = vec![
            json!({"type":"item.completed","item":{"type":"command_execution"}}),
            json!({"type":"turn.completed"}),
        ];
        assert!(parse_cli("codex-cli", "model", &events).is_err());
    }
    #[test]
    fn claude_uses_reported_model() {
        let events = vec![
            json!({"type":"system","subtype":"init","tools":[],"mcp_servers":[]}),
            json!({"type":"assistant","message":{"model":"actual-model","content":[{"type":"text","text":"Prose."}]}}),
            json!({"type":"result","subtype":"success","is_error":false,"result":"Prose."}),
        ];
        let (_, m, _) = parse_cli("claude-cli", "alias", &events).unwrap();
        assert_eq!(m, "actual-model");
    }
}
