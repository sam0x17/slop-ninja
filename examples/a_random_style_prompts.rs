//! Durable local creation and independent-call screening of free-form style prompts.
use anyhow::{Context, Result, ensure};
use reqwest::blocking::Client;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{Read, Write},
    path::Path,
    time::{Duration, Instant},
};
#[allow(dead_code)]
#[path = "a_balanced_generate/style_recipe.rs"]
mod pinned_skill;
#[allow(dead_code)]
#[path = "a_balanced_generate/random_style_recipe.rs"]
mod recipe;
fn sha(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}
fn read(path: &Path) -> Result<Value> {
    Ok(serde_json::from_slice(&fs::read(path)?)?)
}
fn put(path: &Path, value: &Value) -> Result<()> {
    let parent = path.parent().context("parent")?;
    let mut f = tempfile::NamedTempFile::new_in(parent)?;
    serde_json::to_writer_pretty(&mut f, value)?;
    f.write_all(b"\n")?;
    f.as_file().sync_all()?;
    f.persist_noclobber(path)?;
    fs::File::open(parent)?.sync_all()?;
    Ok(())
}
fn file_sha(path: &Path) -> Result<String> {
    let mut f = fs::File::open(path)?;
    let mut h = Sha256::new();
    let mut buf = [0; 65536];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(hex::encode(h.finalize()))
}
#[derive(Debug)]
struct ServerFailure;
impl std::fmt::Display for ServerFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "local server/transport failure; raw receipt retained; stopping collection"
        )
    }
}
impl std::error::Error for ServerFailure {}

fn decode_response(response: &Value) -> Result<Value> {
    if response["status"] != 200 || response.get("read_error").is_some() {
        return Err(ServerFailure.into());
    }
    ensure!(
        response["body"]["choices"][0]["finish_reason"] == "stop",
        "local generation did not complete"
    );
    let text = response["body"]["choices"][0]["message"]["content"]
        .as_str()
        .context("content")?
        .trim();
    let text = if let Some(rest) = text.strip_prefix("<think>") {
        let (thought, body) = rest
            .split_once("</think>")
            .context("reasoning terminator")?;
        ensure!(thought.trim().is_empty(), "nonempty reasoning");
        body.trim()
    } else {
        text
    };
    let text = if let Some(rest) = text.strip_prefix("```json") {
        rest.strip_suffix("```")
            .context("unclosed JSON fence")?
            .trim()
    } else {
        text
    };
    let value: Value = serde_json::from_str(text).context("expected JSON object")?;
    ensure!(value.is_object(), "expected JSON object");
    Ok(value)
}
fn completion(
    client: &Client,
    endpoint: &str,
    alias: &str,
    prompt: &str,
    seed: u64,
    review: bool,
    dir: &Path,
) -> Result<Value> {
    fs::create_dir_all(dir)?;
    let request = json!({"model":alias,"messages":[{"role":"user","content":prompt}],"temperature":if review {0.0}else{0.95},"top_p":0.95,"seed":seed,"max_tokens":if review {256}else{512},"stream":false,"chat_template_kwargs":{"enable_thinking":false},"reasoning_format":"none"});
    let intent = dir.join("intent.json");
    let result = dir.join("response.json");
    if intent.exists() {
        ensure!(
            read(&intent)?["request"] == request,
            "resume request changed"
        );
    }
    if !result.exists() {
        ensure!(
            !intent.exists(),
            "uncertain local request retained; no automatic repeat"
        );
        put(
            &intent,
            &json!({"request":request,"endpoint":endpoint,"created":chrono::Utc::now().to_rfc3339()}),
        )?;
        let clock = Instant::now();
        let response = client
            .post(format!("{endpoint}/v1/chat/completions"))
            .json(&request)
            .send();
        let envelope = match response {
            Ok(r) => {
                let status = r.status().as_u16();
                match r.text() {
                    Ok(raw) => {
                        json!({"status":status,"raw_body":raw,"body":serde_json::from_str::<Value>(&raw).unwrap_or(Value::Null),"elapsed_ms":clock.elapsed().as_millis()})
                    }
                    Err(e) => json!({"status":status,"read_error":e.to_string()}),
                }
            }
            Err(e) => json!({"status":0,"transport_error":e.to_string()}),
        };
        put(&result, &envelope)?;
    }
    decode_response(&read(&result)?)
}
fn main() -> Result<()> {
    let a: Vec<_> = std::env::args().skip(1).collect();
    ensure!(
        a.len() == 4,
        "a_random_style_prompts PLAN.json OUT GENERATOR.json FIX_SLOP_DIR"
    );
    let plan = read(Path::new(&a[0]))?;
    let parent_bytes = fs::read(
        Path::new(&a[0])
            .parent()
            .context("plan parent")?
            .join("humans.jsonl"),
    )?;
    ensure!(
        plan["parents_sha256"] == sha(&parent_bytes),
        "source population differs"
    );
    let mut parents = std::collections::BTreeMap::new();
    for line in std::str::from_utf8(&parent_bytes)?.lines() {
        let row: Value = serde_json::from_str(line)?;
        let text = row["text"].as_str().context("source text")?;
        ensure!(
            row["frozen_split"] == "train" && row["text_sha256"] == sha(text.as_bytes()),
            "source binding differs"
        );
        ensure!(
            parents
                .insert(row["id"].as_str().context("source id")?.to_owned(), row)
                .is_none(),
            "duplicate source id"
        );
    }
    let config = read(Path::new(&a[2]))?;
    ensure!(
        plan["batch"] == recipe::VERSION && plan["split"] == "train",
        "Train random-style acquisition required"
    );
    ensure!(
        config["rights"]["generation_permitted"] == true,
        "generator permission missing"
    );
    let endpoint = config["endpoint"]
        .as_str()
        .context("endpoint")?
        .trim_end_matches('/');
    let url = reqwest::Url::parse(endpoint)?;
    ensure!(
        url.scheme() == "http"
            && url.host_str() == Some("127.0.0.1")
            && url.path() == "/"
            && url.port().is_some()
            && url.query().is_none()
            && url.fragment().is_none()
            && url.username().is_empty()
            && url.password().is_none(),
        "task-owned loopback endpoint required"
    );
    for (path, key) in [("model_path", "sha256"), ("runtime_path", "runtime_sha256")] {
        ensure!(
            file_sha(Path::new(config[path].as_str().context("path")?))? == config[key],
            "generator artifact differs"
        );
    }
    for lib in config["runtime_libraries"]
        .as_array()
        .context("libraries")?
    {
        ensure!(
            file_sha(Path::new(lib["path"].as_str().context("library path")?))? == lib["sha256"],
            "library changed"
        );
    }
    let skill = pinned_skill::load_skill(Path::new(&a[3]))?;
    let out = Path::new(&a[1]);
    fs::create_dir_all(out)?;
    let binding = json!({"schema":"slop_ninja_random_style_prompt_run_v1","prompt_policy":recipe::PROMPT_POLICY,"plan_sha256":sha(&fs::read(&a[0])?),"generator_config":config,"generator_config_sha256":sha(&fs::read(&a[2])?),"producer_sha256":sha(include_bytes!("a_random_style_prompts.rs")),"recipe_sha256":sha(include_bytes!("a_balanced_generate/random_style_recipe.rs")),"fix_slop_bundle_sha256":sha(skill.as_bytes()),"review":"separate call to same pinned model; automatic quality screening, not independent human verification","max_attempts":2,"seed":1729});
    if out.join("binding.json").exists() {
        ensure!(
            read(&out.join("binding.json"))? == binding,
            "resume binding differs"
        );
    } else {
        put(&out.join("binding.json"), &binding)?;
    }
    let client = Client::builder()
        .timeout(Duration::from_secs(120))
        .redirect(reqwest::redirect::Policy::none())
        .retry(reqwest::retry::never())
        .build()?;
    let props: Value = client
        .get(format!("{endpoint}/props"))
        .send()?
        .error_for_status()?
        .json()?;
    ensure!(
        props["model_path"] == config["model_path"],
        "server model differs"
    );
    let models: Value = client
        .get(format!("{endpoint}/v1/models"))
        .send()?
        .error_for_status()?
        .json()?;
    ensure!(
        models["data"]
            .as_array()
            .is_some_and(|r| r.iter().any(|m| m["id"] == config["alias"])),
        "server alias differs"
    );
    let props_binding = json!({"props":props,"models":models});
    if !out.join("server.json").exists() {
        put(&out.join("server.json"), &props_binding)?;
    } else {
        ensure!(
            read(&out.join("server.json"))?["props"] == props_binding["props"],
            "server settings changed"
        );
    }
    let alias = config["alias"].as_str().context("alias")?;
    let mut cells = Vec::new();
    let mut seen = Vec::<String>::new();
    let mut names = std::collections::BTreeSet::new();
    for (index, cell) in plan["cells"]
        .as_array()
        .context("cells")?
        .iter()
        .enumerate()
    {
        ensure!(
            !out.join("PAUSE").exists(),
            "operator pause; all completed prompts retained"
        );
        let dir = out.join(cell["style"].as_str().context("style")?);
        let parent = parents
            .get(cell["parent_id"].as_str().context("parent id")?)
            .context("source absent")?;
        ensure!(
            parent["text_sha256"] == cell["source_sha256"],
            "cell source differs"
        );
        fs::create_dir_all(&dir)?;
        let record = dir.join("record.json");
        let frozen = if record.exists() {
            let r = read(&record)?;
            ensure!(r["cell"] == *cell, "cell differs");
            r
        } else {
            let mut accepted = None;
            let mut errors = Vec::new();
            for attempt in 0..2u64 {
                let generation = dir.join(format!("attempt-{attempt}-create"));
                let review_dir = dir.join(format!("attempt-{attempt}-review"));
                let result = (|| -> Result<Value> {
                    let mut prompt =
                        recipe::creation_prompt(cell, parent["text"].as_str().unwrap(), &seen)?;
                    if attempt > 0 {
                        prompt.push_str("\nA previous independent draw failed quality/uniqueness checks. Invent a different coherent style; follow the length and factual constraints carefully.");
                        prompt
                            .push_str(&format!("\nPrevious rejection: {}", errors.last().unwrap()));
                    }
                    let candidate = completion(
                        &client,
                        endpoint,
                        alias,
                        &prompt,
                        1729 + index as u64 * 2 + attempt,
                        false,
                        &generation,
                    )?;
                    recipe::validate_candidate(&candidate)?;
                    let instruction = candidate["instruction"].as_str().unwrap();
                    let name = candidate["style_name"].as_str().unwrap().to_lowercase();
                    ensure!(
                        !names.contains(&name)
                            && !seen.iter().any(|s| recipe::similar(s, instruction)),
                        "duplicate or near-duplicate style"
                    );
                    let review = completion(
                        &client,
                        endpoint,
                        alias,
                        &recipe::review_prompt(
                            &candidate,
                            cell,
                            parent["text"].as_str().unwrap(),
                            &seen,
                        )?,
                        1729,
                        true,
                        &review_dir,
                    )?;
                    recipe::validate_review(&review)?;
                    Ok(
                        json!({"candidate":candidate,"review":review,"attempt":attempt,"creation_response_sha256":sha(&fs::read(generation.join("response.json"))?),"review_response_sha256":sha(&fs::read(review_dir.join("response.json"))?)}),
                    )
                })();
                match result {
                    Ok(r) => {
                        accepted = Some(r);
                        break;
                    }
                    Err(e) if e.downcast_ref::<ServerFailure>().is_some() => return Err(e),
                    Err(e) => errors.push(json!({"attempt":attempt,"error":e.to_string()})),
                }
            }
            let r = json!({"cell":cell,"accepted":accepted.is_some(),"result":accepted,"errors":errors});
            put(&record, &r)?;
            r
        };
        let mut entry = cell.clone();
        entry["accepted"] = frozen["accepted"].clone();
        entry["receipt_sha256"] = json!(sha(&fs::read(&record)?));
        entry["receipt_path"] = json!(record);
        if frozen["accepted"] == true {
            let candidate = &frozen["result"]["candidate"];
            recipe::validate_candidate(candidate)?;
            let instruction = candidate["instruction"].as_str().unwrap().to_owned();
            let name = candidate["style_name"].as_str().unwrap().to_lowercase();
            ensure!(
                names.insert(name) && !seen.iter().any(|s| recipe::similar(s, &instruction)),
                "retained prompt duplicate"
            );
            seen.push(instruction);
            entry["candidate"] = candidate.clone();
            entry["review"] = frozen["result"]["review"].clone();
        }
        cells.push(entry);
        eprintln!(
            "prompts: {} processed; {} accepted; {} failed",
            index + 1,
            seen.len(),
            index + 1 - seen.len()
        );
    }
    let catalog = json!({"schema":"slop_ninja_random_style_catalog_v1","parents_sha256":plan["parents_sha256"],"plan_sha256":sha(&fs::read(&a[0])?),"binding":binding,"instruction_matrix":plan["instruction_matrix"],"cells":cells,"accepted":seen.len(),"requested":plan["requested"],"fix_slop_bundle":skill,"fix_slop_bundle_sha256":sha(skill.as_bytes()),"style_compliance_verified":false,"fitting_admitted":false});
    let path = out.join("catalog.json");
    if path.exists() {
        ensure!(read(&path)? == catalog, "catalog differs");
    } else {
        put(&path, &catalog)?;
    }
    ensure!(
        !seen.is_empty(),
        "zero usable prompts; catalog retained, no rewriting allowed"
    );
    println!("{}", path.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn server_errors_stop_the_collection_instead_of_rejecting_every_style() {
        for status in [0, 400, 429, 500] {
            assert!(
                decode_response(&json!({"status":status}))
                    .unwrap_err()
                    .downcast_ref::<ServerFailure>()
                    .is_some()
            );
        }
    }
    #[test]
    fn only_complete_json_and_verified_empty_wrappers_are_accepted() {
        let envelope = |text: &str, finish: &str| json!({"status":200,"body":{"choices":[{"finish_reason":finish,"message":{"content":text}}]}});
        assert_eq!(
            decode_response(&envelope(
                "<think>\n</think>\n```json\n{\"accept\":true}\n```",
                "stop"
            ))
            .unwrap(),
            json!({"accept":true})
        );
        assert!(decode_response(&envelope("{\"accept\":true}", "length")).is_err());
        assert!(decode_response(&envelope("Analysis: {\"accept\":true}", "stop")).is_err());
        assert!(
            decode_response(&envelope(
                "<think>hidden reasoning</think>{\"accept\":true}",
                "stop"
            ))
            .is_err()
        );
    }
}
