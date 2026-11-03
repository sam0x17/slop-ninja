//! Replay archived provider CLI streams without executing a provider.
use anyhow::{Context, Result, ensure};
use clap::ValueEnum;
use serde_json::{Value, json};

#[derive(Clone, Copy, ValueEnum)]
pub enum Backend {
    Codex,
    Claude,
}

pub fn parse_capture(backend: Backend, stdout: &[u8]) -> Result<(String, Value)> {
    match backend {
        Backend::Codex => {
            let events: Vec<Value> = std::str::from_utf8(stdout)?
                .lines()
                .map(serde_json::from_str)
                .collect::<std::result::Result<_, _>>()?;
            ensure!(
                !events
                    .iter()
                    .any(|e| e["type"] == "error" || e["type"] == "turn.failed"),
                "Codex reported failure"
            );
            let turns: Vec<_> = events
                .iter()
                .filter(|e| e["type"] == "turn.completed")
                .collect();
            ensure!(turns.len() == 1, "Expected one completed Codex turn");
            let mut messages = Vec::new();
            for event in &events {
                if event["type"] == "item.started" || event["type"] == "item.completed" {
                    let item = &event["item"];
                    ensure!(
                        item["type"] == "agent_message" || item["type"] == "reasoning",
                        "Tool or other non-prose activity in capture"
                    );
                    if event["type"] == "item.completed" && item["type"] == "agent_message" {
                        messages.push(item["text"].as_str().context("Missing Codex prose")?);
                    }
                }
            }
            ensure!(
                messages.len() == 1 && !messages[0].trim().is_empty(),
                "Require one prose response"
            );
            Ok((
                messages[0].to_owned(),
                json!({"reported_models":[],"usage":turns[0]["usage"],"completion_evidence":"one turn.completed; exit zero; one prose message; no tools","immutable_snapshot_exposed":false}),
            ))
        }
        Backend::Claude => {
            let events: Vec<Value> = std::str::from_utf8(stdout)?
                .lines()
                .map(serde_json::from_str)
                .collect::<std::result::Result<_, _>>()?;
            let results: Vec<_> = events.iter().filter(|e| e["type"] == "result").collect();
            ensure!(results.len() == 1, "Require one final Claude result");
            let result = results[0];
            ensure!(
                result["type"] == "result"
                    && result["subtype"] == "success"
                    && result["is_error"] == false
                    && result["num_turns"] == 1,
                "Incomplete Claude capture"
            );
            let text = result["result"]
                .as_str()
                .filter(|s| !s.trim().is_empty())
                .context("Missing Claude prose")?;
            let usage_models: Vec<_> = result["modelUsage"]
                .as_object()
                .context("Missing reported Claude model usage")?
                .keys()
                .cloned()
                .collect();
            let mut models = std::collections::BTreeSet::new();
            let mut prose = String::new();
            for event in events.iter().filter(|e| e["type"] == "assistant") {
                let message = &event["message"];
                for content in message["content"]
                    .as_array()
                    .context("Missing assistant content")?
                {
                    ensure!(
                        content["type"] == "text"
                            || content["type"] == "thinking"
                            || content["type"] == "redacted_thinking",
                        "Non-prose Claude tool content"
                    );
                    if content["type"] == "text" {
                        prose.push_str(content["text"].as_str().context("Missing assistant text")?);
                        models.insert(
                            message["model"]
                                .as_str()
                                .context("Missing prose-generating model")?
                                .to_owned(),
                        );
                    }
                }
            }
            ensure!(
                models.len() == 1 && prose == text,
                "Assistant prose/model differs from final result"
            );
            let usage_only_models: Vec<_> = usage_models
                .into_iter()
                .filter(|m| !models.contains(m))
                .collect();
            Ok((
                text.to_owned(),
                json!({"reported_models":models,"usage_only_models":usage_only_models,"usage":result["usage"],"model_usage":result["modelUsage"],"reported_cost_usd":result["total_cost_usd"],"completion_evidence":"successful single-turn result matches assistant prose; prose model from assistant message; tools disabled","immutable_snapshot_exposed":false}),
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn codex_capture_rejects_tool_use_and_missing_completion() {
        let good = b"{\"type\":\"item.completed\",\"item\":{\"type\":\"agent_message\",\"text\":\"Synthetic prose.\"}}\n{\"type\":\"turn.completed\",\"usage\":{}}";
        assert_eq!(
            parse_capture(Backend::Codex, good).unwrap().0,
            "Synthetic prose."
        );
        assert!(parse_capture(Backend::Codex, b"{\"type\":\"turn.started\"}").is_err());
        let mut with_tool = good.to_vec();
        with_tool.extend_from_slice(
            b"\n{\"type\":\"item.completed\",\"item\":{\"type\":\"command_execution\"}}",
        );
        assert!(parse_capture(Backend::Codex, &with_tool).is_err());
    }

    #[test]
    fn claude_prose_identity_is_separate_from_usage_only_models() {
        let assistant = json!({"type":"assistant","message":{"model":"main-model","content":[{"type":"text","text":"Synthetic prose."}]}});
        let result = json!({"type":"result","subtype":"success","is_error":false,"num_turns":1,"result":"Synthetic prose.","modelUsage":{"main-model":{},"helper-model":{}}});
        let bytes = format!("{assistant}\n{result}");
        let (_, metadata) = parse_capture(Backend::Claude, bytes.as_bytes()).unwrap();
        assert_eq!(metadata["reported_models"], json!(["main-model"]));
        assert_eq!(metadata["usage_only_models"], json!(["helper-model"]));
        assert!(parse_capture(Backend::Claude, result.to_string().as_bytes()).is_err());
    }
}
