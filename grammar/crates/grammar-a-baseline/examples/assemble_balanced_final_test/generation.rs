//! Test scope around the original v2 recipe; all task attempts remain retained.
use super::*;
pub(super) fn check_slot(slot: usize, c: &Value, split: &str) -> Result<()> {
    ensure!(
        slot < 3
            && split == "test"
            && c["schema"] == "slop_ninja_local_generator_v1"
            && c["identity"] == MODELS[slot]
            && c["rights"]["generation_permitted"] == true,
        "frozen Test generator identity/rights"
    );
    ensure!(
        c["allowed_splits"] == json!(["test"])
            || (slot < 2
                && c["allowed_splits"] == json!(["train", "development", "calibration", "test"])),
        "Test generator allowed splits"
    );
    ensure!(
        str_at(c, "identity")?.ends_with(digest_at(c, "sha256")?),
        "model identity/weight digest"
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
    ensure!(
        endpoint
            .strip_prefix("http://127.0.0.1:")
            .is_some_and(|v| v.parse::<u16>().is_ok_and(|p| p > 0)),
        "loopback endpoint required"
    );
    ensure!(
        !array(c, "runtime_libraries")?.is_empty(),
        "runtime libraries required"
    );
    for r in array(c, "runtime_libraries")? {
        str_at(r, "path")?;
        digest_at(r, "sha256")?;
    }
    Ok(())
}
pub(super) fn check_plan(
    p: &Value,
    parent: &Value,
    binding: &Value,
    percent: u64,
    id: &str,
) -> Result<()> {
    balanced::check_plan(p, parent, binding, percent, id)?;
    ensure!(
        parent["purpose"] == "query"
            && parent["frozen_split"] == "test"
            && PERCENTAGES.contains(&percent),
        "reserved Test query recipe"
    );
    let t = str_at(parent, "text")?;
    let pos =
        Sha256::digest(format!("{}:{percent}", str_at(parent, "id")?).as_bytes())[0] as usize % 3;
    let (s, e) = if percent == 0 {
        (0, 0)
    } else {
        recipe::span(t, percent as usize, pos)
    };
    ensure!(
        p["source_byte_span"] == json!([s, e]),
        "exact deterministic v2 source span"
    );
    let prompt = if percent == 0 {
        recipe::scratch_prompt(str_at(parent, "id")?)
    } else {
        format!(
            "Rewrite the following English narrative passage in your own words, keeping its events, perspective, and approximate length. Return only the replacement prose in one paragraph: no title, preface, analysis, quotation delimiters, or explanation.\n\n{}",
            &t[s..e]
        )
    };
    ensure!(
        p["request"]
            == json!({"model":binding["generator_config"]["alias"],"messages":[{"role":"user","content":prompt}],
        "temperature":0.7,"top_p":0.9,"seed":1729,"max_tokens":1024,"stream":false,
        "chat_template_kwargs":{"enable_thinking":false},"reasoning_format":"none"}),
        "exact original semantic request"
    );
    Ok(())
}
pub(super) fn read(
    spec: &Value,
    slot: usize,
    config: &Value,
    humans: &[Value],
    human_sha: &str,
    producer: &str,
    inputs: &mut Vec<Value>,
    prompts: &mut BTreeMap<(String, u64), Value>,
) -> Result<(Vec<Value>, Value)> {
    let dir = Path::new(str_at(spec, "run_directory")?);
    let config_sha = digest_at(&spec["config"], "sha256")?;
    let (report, _) = load(&dir.join("report.json"), inputs)?;
    let (binding, _) = load(&dir.join("binding.json"), inputs)?;
    ensure!(
        report["schema"] == "slop_ninja_controlled_generation_report_v2"
            && report["complete"] == true
            && report["test_opened"] == true
            && report["input_sha256"] == human_sha
            && report["binding_sha256"] == compact_sha(&binding)?,
        "complete Test report binding"
    );
    ensure!(
        binding["schema"] == "slop_ninja_controlled_generation_v2"
            && binding["test_opened"] == true
            && binding["input_sha256"] == human_sha
            && binding["generator"] == MODELS[slot]
            && binding["generator_config"] == *config
            && binding["generator_config_sha256"] == config_sha
            && binding["producer_sha256"] == producer
            && binding["requested_span_percentages"] == json!([20, 60, 100])
            && binding["share_word_policy"] == "unicode_split_whitespace_v1"
            && binding["span_policy"] == "nearest_sentence_start_middle_end_by_source_digest_v2",
        "original v2 generation binding"
    );
    let rb = bytes(&dir.join("records.jsonl"))?;
    ensure!(
        report["records_sha256"] == sha(&rb),
        "generated aggregate hash"
    );
    inputs.push(json!({"path":dir.join("records.jsonl"),"sha256":sha(&rb)}));
    let generated = records(&rb)?;
    let accepted = unique_rows(&generated)?;
    ensure!(
        report["accepted"] == accepted.len(),
        "generation accepted count"
    );
    let mut expected = BTreeMap::new();
    for parent in humans {
        ensure!(
            parent["frozen_split"] == "test" && parent["purpose"] == "query",
            "non-Test/reference generator root"
        );
        for percent in PERCENTAGES {
            let id = balanced::task_id(str_at(parent, "id")?, MODELS[slot], percent);
            ensure!(
                expected.insert(id, (parent, percent)).is_none(),
                "duplicate generation task"
            );
        }
    }
    ensure!(
        expected.len() == 4 * humans.len() && expected.len() <= 128,
        "requested task count"
    );
    let failures = array(&report, "failures")?;
    let failed = unique_rows(failures)?;
    balanced::exact_outcomes(
        &expected.keys().cloned().collect(),
        &accepted.keys().cloned().collect(),
        &failed.keys().cloned().collect(),
    )?;
    for (id, (parent, percent)) in &expected {
        let task = dir.join(id);
        let (plan, pb) = load(&task.join("plan.json"), inputs)?;
        check_plan(&plan, parent, &binding, *percent, id)?;
        let mut semantic = plan["request"].clone();
        semantic.as_object_mut().context("request")?.remove("model");
        if let Some(prior) =
            prompts.insert((str_at(parent, "id")?.into(), *percent), semantic.clone())
        {
            ensure!(prior == semantic, "different semantic tasks across models");
        }
        let (context, _) = load(&task.join("context.json"), inputs)?;
        let tokens = counter(&context, "prompt_tokens")?;
        ensure!(
            context["request_sha256"] == compact_sha(&plan["request"])?
                && context["max_output_tokens"] == 1024
                && context["context_capacity"] == 8192
                && context["fits"] == (tokens.checked_add(1024).context("token overflow")? < 8192),
            "context receipt binding"
        );
        let mut responses = BTreeMap::new();
        let mut intents = 0;
        for n in 0..2 {
            let ip = task.join(format!("attempt-{n}.intent.json"));
            let rp = task.join(format!("attempt-{n}.json"));
            if ip.exists() {
                let (i, _) = load(&ip, inputs)?;
                ensure!(
                    i["request"] == plan["request"]
                        && i["plan_sha256"] == sha(&pb)
                        && i["attempt"] == n,
                    "attempt intent binding"
                );
                intents += 1;
            } else {
                ensure!(!rp.exists(), "response without intent");
            }
            if rp.exists() {
                let (r, b) = load(&rp, inputs)?;
                responses.insert(n, (r, sha(&b)));
            }
        }
        if let Some(row) = accepted.get(id) {
            ensure!(context["fits"] == true, "accepted unsupported context");
            balanced::validate_descendant_with_slot(
                row, parent, config, &binding, &plan, slot, *percent, check_slot,
            )?;
            let (saved, b) = load(&task.join("record.json"), inputs)?;
            let (hashes, _) = load(&task.join("record.sha256.json"), inputs)?;
            ensure!(
                saved == **row && hashes["sha256"] == sha(&b) && hashes["plan_sha256"] == sha(&pb),
                "saved record/plan digest"
            );
            let n = counter(&row["generation"], "attempt")?;
            let (r, rsha) = responses.get(&n).context("missing accepted response")?;
            ensure!(
                Path::new(str_at(&row["generation"], "response_path")?).canonicalize()?
                    == task.join(format!("attempt-{n}.json")).canonicalize()?
                    && row["generation"]["response_sha256"] == *rsha,
                "response path/digest"
            );
            let replacement = evidence::replacement(r)?.context("response not acceptable")?;
            ensure!(
                row["share_evidence"]["replacement"] == replacement,
                "response/final replacement differs"
            );
        } else {
            let f = failed[id];
            ensure!(f["parent_id"] == parent["id"], "failed parent binding");
            match str_at(f, "reason")? {
                "unsupported_context" => ensure!(
                    context["fits"] == false && f["context"] == context && intents == 0,
                    "context failure evidence"
                ),
                "no_accepted_response_after_bounded_attempts" => ensure!(
                    context["fits"] == true
                        && intents == 2
                        && f["mode"] == plan["mode"]
                        && f["requested_span_percent"] == *percent,
                    "terminal attempt evidence"
                ),
                _ => anyhow::bail!("unknown terminal failure"),
            }
        }
    }
    evidence::evidence(dir, spec, config, humans, &generated, inputs)?;
    let summary = json!({"slot":SLOTS[slot],"generator":MODELS[slot],"planned_requested":128,
        "requested":expected.len(),"not_requested_due_to_extraction":128-expected.len(),"accepted":accepted.len(),
        "failed":failed.len(),"failures":failures,"complete":true,"input_sha256":human_sha,
        "config_sha256":config_sha,"binding_sha256":compact_sha(&binding)?,"report":report});
    Ok((generated, summary))
}
