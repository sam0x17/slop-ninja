//! Durable local generation with exact replacement-span supervision.
//! Corpus/provenance outputs contain prose and belong in ignored task storage.
use anyhow::{Context, Result, ensure};
use reqwest::blocking::Client;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::Path,
    time::{Duration, Instant},
};

const MODEL: &str =
    "Qwen3.6-35B-A3B:649d7508507b84638732c4f52c24c8b15843c6dca2f3ff793ae07c14a67ebbb3";
fn sha(b: &[u8]) -> String {
    hex::encode(Sha256::digest(b))
}
fn put(path: &Path, v: &Value) -> Result<()> {
    let mut f = OpenOptions::new().write(true).create_new(true).open(path)?;
    serde_json::to_writer_pretty(&mut f, v)?;
    f.write_all(b"\n")?;
    f.sync_all()?;
    Ok(())
}
fn read(path: &Path) -> Result<Value> {
    Ok(serde_json::from_slice(&fs::read(path)?)?)
}
fn words(s: &str) -> usize {
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
fn span(text: &str, percent: usize) -> (usize, usize) {
    if percent == 100 {
        return (0, text.len());
    }
    let desired = (words(text) * percent / 100).max(1);
    let mut boundaries = Vec::new();
    for (i, c) in text.char_indices() {
        if matches!(c, '.' | '?' | '!') {
            let end = i + c.len_utf8();
            if end == text.len() || text[end..].starts_with(char::is_whitespace) {
                boundaries.push(end);
            }
        }
    }
    if boundaries.is_empty() {
        boundaries.extend(
            text.char_indices()
                .filter_map(|(i, c)| c.is_whitespace().then_some(i)),
        );
    }
    let end = boundaries
        .into_iter()
        .filter(|&e| e > 0 && e < text.len())
        .min_by_key(|&e| (words(&text[..e]).abs_diff(desired), e))
        .unwrap_or(text.len());
    (0, end)
}

fn assembled(
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
    ensure!(
        a.len() == 3 || (a.len() == 4 && a[3] == "--test"),
        "a_controlled_generate HUMANS.jsonl OUTPUT LOOPBACK_BASE_URL [--test]"
    );
    let test_mode = a.len() == 4;
    ensure!(
        matches!(a[2].as_str(), "http://127.0.0.1:18089"),
        "only task-owned loopback server allowed"
    );
    let input = fs::read(&a[0])?;
    let out = Path::new(&a[1]);
    fs::create_dir_all(out)?;
    let binding = json!({"schema":"slop_ninja_controlled_generation_v1", "input_sha256":sha(&input),"generator":MODEL,
        "repo":"unsloth/Qwen3.6-35B-A3B-GGUF", "revision":"a483e9e6cbd595906af30beda3187c2663a1118c",
        "runtime":"llama.cpp-b11756-CUDA12.8", "endpoint":a[2], "share_word_policy":"unicode_split_whitespace_v1",
        "requested_span_percentages":[25,75,100], "seed":1729, "temperature":0.7, "top_p":0.9,
        "max_tokens":1024,"thinking":false,"input_schema":"slop_ninja_a_supervised_record_v1",
        "failures":"retained per attempt; maximum two requests per item; no substitute labels", "test_opened":test_mode});
    let binding_path = out.join("binding.json");
    if binding_path.exists() {
        ensure!(read(&binding_path)? == binding, "resume binding differs");
    } else {
        put(&binding_path, &binding)?;
    }
    let client = Client::builder()
        .timeout(Duration::from_secs(180))
        .build()?;
    let mut accepted = Vec::new();
    let mut failures = Vec::new();
    for line in std::str::from_utf8(&input)?.lines() {
        let source: Value = serde_json::from_str(line)?;
        ensure!(source["origin"] == "human_only", "human source required");
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
        accepted.push(source.clone());
        if source["purpose"] == "reference" {
            continue;
        }
        for percent in [0, 25, 75, 100] {
            let mode = if percent == 0 {
                "from_scratch"
            } else {
                "controlled_rewrite"
            };
            let generation_id = format!(
                "local-{}",
                &sha(format!("{id}:{MODEL}:{mode}:{percent}").as_bytes())[..24]
            );
            let dir = out.join(&generation_id);
            fs::create_dir_all(&dir)?;
            let done = dir.join("record.json");
            if done.exists() {
                let r = read(&done)?;
                ensure!(
                    r["parent_id"] == id && r["ai_identity"] == MODEL,
                    "resume record binding"
                );
                accepted.push(r);
                continue;
            }
            let (start, end) = if percent == 0 {
                (0, 0)
            } else {
                span(text, percent)
            };
            let prompt = if percent == 0 {
                let digest = Sha256::digest(id.as_bytes());
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
                format!(
                    "Write an original English fictional scene of about 220 words set in {} involving {}. Use natural narrative prose. Return only the scene, with no title, analysis, introduction, or explanation.",
                    places[digest[0] as usize % 8],
                    events[digest[1] as usize % 8]
                )
            } else {
                format!(
                    "Rewrite the following English narrative passage in your own words, keeping its events, perspective, and approximate length. Return only the replacement prose in one paragraph: no title, preface, analysis, quotation delimiters, or explanation.\n\n{}",
                    &text[start..end]
                )
            };
            let request = json!({"model":"a-supervised-qwen","messages":[{"role":"user","content":prompt}],
                "temperature":0.7,"top_p":0.9,"seed":1729,"max_tokens":1024,"stream":false,
                "chat_template_kwargs":{"enable_thinking":false},"reasoning_format":"none"});
            let plan = json!({"id":generation_id,"parent_id":id,"source_sha256":source["text_sha256"],
                "mode":mode,"requested_source_span_percent":percent,"source_byte_span":[start,end],"request":request,
                "sham_reassembly_exact": percent==0 || format!("{}{}{}",&text[..start],&text[start..end],&text[end..])==text});
            let path = dir.join("plan.json");
            if path.exists() {
                ensure!(read(&path)? == plan, "generation plan differs");
            } else {
                put(&path, &plan)?;
            }
            let mut result = None;
            for attempt in 0..2 {
                let path = dir.join(format!("attempt-{attempt}.json"));
                let response = if path.exists() {
                    read(&path)?
                } else {
                    let clock = Instant::now();
                    let r = client
                        .post(format!("{}/v1/chat/completions", a[2]))
                        .json(&request)
                        .send();
                    let v = match r {
                        Ok(response) => {
                            let status = response.status().as_u16();
                            let body = response.text()?;
                            json!({"status":status,"body":serde_json::from_str::<Value>(&body).unwrap_or(json!({"unparsed":body})),"elapsed_ms":clock.elapsed().as_millis()})
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
                row["ai_identity"] = MODEL.into();
                row["share_target"] = share.into();
                row["share_evidence"] = json!({"schema":"slop_ninja_realized_span_share_v1","source_text":text,
                    "source_sha256":source["text_sha256"],"source_byte_span":[start,end],"replacement":replacement,
                    "final_byte_span":[fs,fe],"final_sha256":row["text_sha256"],"replacement_words":words(replacement),
                    "final_words":words(&final_text),"word_policy":"unicode_split_whitespace_v1","realized_share":share,
                    "requested_percent_not_target":percent,"full_rewrite_is_mixed_history":percent==100});
                row["generation"] = json!({"generator":MODEL,"binding":binding,"request":request,"response_path":path,
                    "response_sha256":sha(&fs::read(&path)?),"mode":mode,"attempt":attempt,
                    "response_format_policy":"strip_verified_empty_leading_think_block_then_trim_v1"});
                put(&done, &row)?;
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
        let mut f = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&final_path)?;
        f.write_all(&bytes)?;
        f.sync_all()?;
    }
    let report = json!({"schema":"slop_ninja_controlled_generation_report_v1","input_sha256":sha(&input),
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
        let (s, e) = span(t, 25);
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
}
