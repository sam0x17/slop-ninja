//! Additional default-v2 recipe, runtime and terminal-attempt checks. Shared
//! lineage, context, response hashes and realized-share arithmetic stay in the
//! balanced assembler and production supervision module.
use super::*;

pub(super) fn check_slot(slot: usize, c: &Value, split: &str) -> Result<()> {
    ensure!(
        slot < 2
            && split == "train"
            && c["schema"] == "slop_ninja_local_generator_v1"
            && c["identity"] == MODELS[slot]
            && c["allowed_splits"] == json!(["train"])
            && c["rights"]["generation_permitted"] == true,
        "known Train-only generator scope"
    );
    ensure!(
        str_at(c, "identity")?.ends_with(digest_at(c, "sha256")?),
        "model identity/digest"
    );
    for k in [
        "repo",
        "revision",
        "filename",
        "model_path",
        "runtime_path",
        "alias",
    ] {
        str_at(c, k)?;
    }
    digest_at(c, "runtime_sha256")?;
    str_at(&c["rights"], "model_license")?;
    let endpoint = str_at(c, "endpoint")?.trim_end_matches('/');
    let port = endpoint
        .strip_prefix("http://127.0.0.1:")
        .context("task-owned loopback endpoint")?;
    ensure!(port.parse::<u16>().is_ok_and(|p| p > 0), "loopback port");
    let libraries = array(c, "runtime_libraries")?;
    ensure!(!libraries.is_empty(), "runtime library evidence required");
    for l in libraries {
        str_at(l, "path")?;
        digest_at(l, "sha256")?;
    }
    Ok(())
}
pub(super) fn check_server(actual: &Value, frozen: &Value, c: &Value) -> Result<()> {
    ensure!(
        actual["model_path"] == c["model_path"]
            && actual["total_slots"] == 1
            && actual["default_generation_settings"]["n_ctx"] == 8192,
        "server model/slots/context"
    );
    str_at(actual, "chat_template")?;
    for key in [
        "model_path",
        "chat_template",
        "chat_template_caps",
        "total_slots",
        "default_generation_settings",
        "build_info",
        "bos_token",
        "eos_token",
    ] {
        ensure!(
            actual.get(key).is_some() && actual[key] == frozen[key],
            "server/template/runtime default changed: {key}"
        );
    }
    Ok(())
}
pub(super) fn check_plan(
    plan: &Value,
    parent: &Value,
    binding: &Value,
    percent: u64,
    id: &str,
) -> Result<()> {
    balanced::check_plan(plan, parent, binding, percent, id)?;
    ensure!(
        parent["frozen_split"] == "train"
            && parent["purpose"] == "query"
            && PERCENTAGES.contains(&percent),
        "book task scope/recipe"
    );
    let text = str_at(parent, "text")?;
    let position =
        Sha256::digest(format!("{}:{percent}", str_at(parent, "id")?).as_bytes())[0] as usize % 3;
    let (s, e) = if percent == 0 {
        (0, 0)
    } else {
        recipe::span(text, percent as usize, position)
    };
    ensure!(
        plan["source_byte_span"] == json!([s, e]),
        "deterministic requested source span differs"
    );
    let prompt = if percent == 0 {
        recipe::scratch_prompt(str_at(parent, "id")?)
    } else {
        format!(
            "Rewrite the following English narrative passage in your own words, keeping its events, perspective, and approximate length. Return only the replacement prose in one paragraph: no title, preface, analysis, quotation delimiters, or explanation.\n\n{}",
            &text[s..e]
        )
    };
    let request = json!({"model":binding["generator_config"]["alias"],"messages":[{"role":"user","content":prompt}],
        "temperature":0.7,"top_p":0.9,"seed":1729,"max_tokens":1024,"stream":false,
        "chat_template_kwargs":{"enable_thinking":false},"reasoning_format":"none"});
    ensure!(
        plan["request"] == request,
        "exact default-v2 narrative request differs"
    );
    Ok(())
}
pub(super) fn replacement(r: &Value) -> Result<Option<&str>> {
    counter(r, "elapsed_ms")?;
    let status = counter(r, "status")?;
    ensure!(
        status == 0 || (100..=599).contains(&status),
        "invalid HTTP status"
    );
    if let Some(raw) = r["raw_body"].as_str() {
        ensure!(
            serde_json::from_str::<Value>(raw).unwrap_or(Value::Null) == r["body"],
            "raw/parsed response differs"
        );
    } else {
        ensure!(
            (status == 0 && r["error"].is_string()) || r["body_read_error"].is_string(),
            "response lacks raw/error evidence"
        );
        return Ok(None);
    }
    if status != 200 || r["body"]["choices"][0]["finish_reason"] != "stop" {
        return Ok(None);
    }
    let Some(content) = r["body"]["choices"][0]["message"]["content"].as_str() else {
        return Ok(None);
    };
    let Ok(t) = balanced::prose(content) else {
        return Ok(None);
    };
    Ok((t.split_whitespace().count() >= 8 && !t.contains("<think>")).then_some(t))
}
pub(super) fn attempts(task: &Path, row: Option<&Value>, inputs: &mut Vec<Value>) -> Result<()> {
    let mut intents = BTreeSet::new();
    let mut responses = BTreeMap::new();
    for entry in fs::read_dir(task)? {
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
    for n in 0..2u64 {
        if task.join(format!("attempt-{n}.intent.json")).exists() {
            intents.insert(n);
        }
        let path = task.join(format!("attempt-{n}.json"));
        if path.exists() {
            ensure!(intents.contains(&n), "response without durable intent");
            let (v, _) = load(&path, inputs)?;
            replacement(&v)?;
            responses.insert(n, v);
        }
    }
    ensure!(
        !intents.contains(&1) || intents.contains(&0),
        "noncontiguous attempts"
    );
    if let Some(row) = row {
        let n = counter(&row["generation"], "attempt")?;
        ensure!(
            n < 2 && intents == (0..=n).collect(),
            "accepted attempt sequence"
        );
        for (_, r) in responses.range(..n) {
            ensure!(
                replacement(r)?.is_none(),
                "earlier accepted response skipped"
            );
        }
        ensure!(
            replacement(responses.get(&n).context("accepted response absent")?)?.is_some(),
            "invalid accepted response"
        );
    } else {
        for r in responses.values() {
            ensure!(
                replacement(r)?.is_none(),
                "terminal failure contains acceptable response"
            );
        }
        ensure!(
            !task.join("record.json").exists() && !task.join("record.sha256.json").exists(),
            "failed task has completed evidence"
        );
    }
    let uncertain = intents
        .iter()
        .filter(|n| !responses.contains_key(n))
        .copied()
        .collect::<Vec<_>>();
    inputs.push(json!({"task":task,"intent_count":intents.len(),"response_count":responses.len(),"uncertain_attempt_indices":uncertain}));
    Ok(())
}
pub(super) fn evidence(
    dir: &Path,
    spec: &Value,
    config: &Value,
    humans: &[Value],
    rows: &[Value],
    inputs: &mut Vec<Value>,
) -> Result<()> {
    let frozen = pinned_json(&spec["server_properties"], inputs)?;
    let (actual, _) = load(&dir.join("server-properties.json"), inputs)?;
    check_server(&actual, &frozen, config)?;
    // Rehash runtime executables/libraries, never load or execute model weights.
    pinned(
        &json!({"path":config["runtime_path"],"sha256":config["runtime_sha256"]}),
        inputs,
    )?;
    for library in array(config, "runtime_libraries")? {
        pinned(library, inputs)?;
    }
    let (binding, _) = load(&dir.join("binding.json"), inputs)?;
    ensure!(
        binding.get("recipe").is_none()
            && binding.get("scratch_policy").is_none()
            && binding.get("rewrite_policy").is_none()
            && binding["seed"] == 1729
            && binding["temperature"] == 0.7
            && binding["top_p"] == 0.9
            && binding["max_tokens"] == 1024
            && binding["thinking"] == false
            && binding["input_schema"] == "slop_ninja_a_supervised_record_v1"
            && binding["endpoint"] == str_at(config, "endpoint")?.trim_end_matches('/'),
        "default-v2 sampler/runtime binding"
    );
    let accepted = unique_rows(rows)?;
    let mut expected = BTreeSet::new();
    for human in humans {
        for percent in PERCENTAGES {
            let id = balanced::task_id(str_at(human, "id")?, str_at(config, "identity")?, percent);
            let task = dir.join(&id);
            expected.insert(id.clone());
            let (context, _) = load(&task.join("context.json"), inputs)?;
            digest_at(&context, "rendered_prompt_sha256")?;
            let row = accepted.get(&id).copied();
            attempts(&task, row, inputs)?;
            if let Some(row) = row {
                ensure!(
                    row["metadata"] == human["metadata"]
                        && row["generation"]["response_format_policy"]
                            == "strip_verified_empty_leading_think_block_then_trim_v1"
                        && row["evidence"]
                            == if percent == 0 {
                                "recorded_local_generation_from_scratch"
                            } else {
                                "recorded_local_rewrite_of_historical_human_proxy"
                            },
                    "descendant evidence/metadata changed"
                );
            }
        }
    }
    let actual_tasks = fs::read_dir(dir)?
        .map(|e| e.map(|e| e.file_name().to_string_lossy().into_owned()))
        .collect::<std::io::Result<Vec<_>>>()?
        .into_iter()
        .filter(|n| n.starts_with("local-"))
        .collect::<BTreeSet<_>>();
    ensure!(
        actual_tasks == expected,
        "missing/orphan generation task directories"
    );
    Ok(())
}
