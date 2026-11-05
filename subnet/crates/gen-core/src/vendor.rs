//! Lanes, vendors, effort levels, and the effort levels each vendor documents
//! for each model of the roster.
//!
//! "Effort" is a level the vendor's API documents for that model; a model with
//! no effort parameter has the single level [`Effort::Default`]. The decision
//! of 2026-11-05 spreads every model's documents equally over all of its
//! documented levels, so no model's level list is a choice made here: the
//! table below is the vendors' (pages fetched 2026-11-04), and the manifest
//! must list exactly it.

use serde::{Deserialize, Serialize};

/// The generation lane: the frontier half or the Workers AI half.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Lane {
    Frontier,
    WorkersAi,
}

impl Lane {
    pub const ALL: [Lane; 2] = [Lane::Frontier, Lane::WorkersAi];
    pub fn code(self) -> &'static str {
        match self {
            Lane::Frontier => "frontier",
            Lane::WorkersAi => "workers_ai",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Vendor {
    Openai,
    Anthropic,
    WorkersAi,
}

impl Vendor {
    pub fn code(self) -> &'static str {
        match self {
            Vendor::Openai => "openai",
            Vendor::Anthropic => "anthropic",
            Vendor::WorkersAi => "workers_ai",
        }
    }
    pub fn lane(self) -> Lane {
        match self {
            Vendor::Openai | Vendor::Anthropic => Lane::Frontier,
            Vendor::WorkersAi => Lane::WorkersAi,
        }
    }
    /// The vendor's API endpoint (fixed: the generation Worker never takes a
    /// URL from its caller). Workers AI runs through the eval Worker's `AI`
    /// binding and has none.
    pub fn endpoint(self) -> Option<&'static str> {
        match self {
            Vendor::Openai => Some("https://api.openai.com/v1/responses"),
            Vendor::Anthropic => Some("https://api.anthropic.com/v1/messages"),
            Vendor::WorkersAi => None,
        }
    }
}

/// An effort level. `Default` is the one level of a model without an effort
/// parameter.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Effort {
    Default,
    None,
    Low,
    Medium,
    High,
    Xhigh,
    Max,
}

impl Effort {
    pub fn code(self) -> &'static str {
        match self {
            Effort::Default => "default",
            Effort::None => "none",
            Effort::Low => "low",
            Effort::Medium => "medium",
            Effort::High => "high",
            Effort::Xhigh => "xhigh",
            Effort::Max => "max",
        }
    }
    pub fn parse(s: &str) -> Option<Effort> {
        Some(match s {
            "default" => Effort::Default,
            "none" => Effort::None,
            "low" => Effort::Low,
            "medium" => Effort::Medium,
            "high" => Effort::High,
            "xhigh" => Effort::Xhigh,
            "max" => Effort::Max,
            _ => return None,
        })
    }
}

/// The service tier a call ran on (and is priced at).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Tier {
    Standard,
    /// OpenAI Flex: synchronous at Batch prices.
    Flex,
    Batch,
}

/// How an Anthropic model's thinking is configured at each level (the
/// claude-api reference and the model pages, 2026-11-04).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AnthropicThinking {
    /// Haiku 5.5: `none` is `thinking: {type: "disabled"}` at the default
    /// effort (disabled is accepted only at effort high or below; the
    /// default is medium); other levels omit `thinking` and send the effort.
    DisabledAtNone,
    /// Sonnet 5.5: `none` is `thinking: {type: "between_tools"}` with no
    /// other thinking field and no effort (the default, high; between_tools
    /// is a 400 at xhigh and max); `disabled` is a 400.
    BetweenToolsAtNone,
    /// Opus 5.5 and Fable 5.1: thinking is always on; `thinking` is omitted
    /// and the effort always sent (Opus's default is medium).
    AlwaysOn,
}


/// The kind of a generated document.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DocKind {
    /// A span of a human parent rewritten and spliced back.
    Mixed,
    /// Written from scratch from a register and a topic.
    ModelOnly,
}

impl DocKind {
    pub fn code(self) -> &'static str {
        match self {
            DocKind::Mixed => "mixed",
            DocKind::ModelOnly => "model_only",
        }
    }
}

use Effort::{Default as D, High as H, Low as L, Max as X, Medium as M, None as N, Xhigh as XH};

const OPENAI_NONE_TO_MAX: &[Effort] = &[N, L, M, H, XH, X];
const LOW_TO_MAX: &[Effort] = &[L, M, H, XH, X];
const GPT_OSS: &[Effort] = &[L, M, H];
const DEEPSEEK_V4: &[Effort] = &[N, L, H, X];
const KIMI: &[Effort] = &[N, H];
const ONE: &[Effort] = &[D];

/// The vendor-documented effort levels of `model`, in order, for every model
/// the roster may name; `None` for a model this build has no record of.
pub fn documented_efforts(model: &str) -> Option<&'static [Effort]> {
    Some(match model {
        "gpt-6-luna" | "gpt-6.1-terra" => OPENAI_NONE_TO_MAX,
        "gpt-6.1-sol" | "gpt-6-astra" => LOW_TO_MAX,
        "claude-haiku-5-5" | "claude-sonnet-5-5" => OPENAI_NONE_TO_MAX,
        "claude-opus-5-5" | "claude-fable-5-1" => LOW_TO_MAX,
        "@cf/openai/gpt-oss-20b" | "@cf/openai/gpt-oss-120b" => GPT_OSS,
        "@cf/deepseek-ai/deepseek-v4.1-flash-1017" => DEEPSEEK_V4,
        "@cf/moonshotai/kimi-k2.6" => KIMI,
        "@cf/ibm-granite/granite-4.0-h-micro"
        | "@cf/meta/llama-3.2-3b-instruct"
        | "@cf/meta/llama-3.1-8b-instruct-fp8-fast"
        | "@cf/meta/llama-3.1-8b-instruct-fp8"
        | "@cf/meta/llama-3.2-11b-vision-instruct"
        | "@cf/daily-co/phonellm-mini-alpha-2"
        | "@cf/google/gemma-4-26b-a4b-it"
        | "@cf/qwen/qwen3-30b-a3b-fp8"
        | "@cf/mistralai/mistral-small-3.1-24b-instruct"
        | "@cf/meta/llama-4-scout-17b-16e-instruct"
        | "@cf/meta/llama-3.3-70b-instruct-fp8-fast"
        | "@cf/nvidia/nemotron-3-120b-a12b" => ONE,
        _ => return None,
    })
}

/// The thinking rule of an Anthropic model, `None` for any other model.
pub fn anthropic_thinking(model: &str) -> Option<AnthropicThinking> {
    Some(match model {
        "claude-haiku-5-5" => AnthropicThinking::DisabledAtNone,
        "claude-sonnet-5-5" => AnthropicThinking::BetweenToolsAtNone,
        "claude-opus-5-5" | "claude-fable-5-1" => AnthropicThinking::AlwaysOn,
        _ => return None,
    })
}

/// Workers AI ids that are deprecated or excluded and never enter a v2
/// manifest: the two deprecated on 2026-05-30, and the GLM family (CAL-only).
pub fn excluded_workers_ai(model: &str) -> bool {
    matches!(model, "@cf/mistral/mistral-7b-instruct-v0.1" | "@cf/google/gemma-3-12b-it") || model.starts_with("@cf/zai-org/glm-")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frontier_effort_cells_total_44_and_workers_ai_22() {
        let f = [
            "gpt-6-luna", "gpt-6.1-terra", "gpt-6.1-sol", "gpt-6-astra",
            "claude-haiku-5-5", "claude-sonnet-5-5", "claude-opus-5-5", "claude-fable-5-1",
        ];
        assert_eq!(f.iter().map(|m| documented_efforts(m).unwrap().len()).sum::<usize>(), 44);
        let w = [
            "@cf/ibm-granite/granite-4.0-h-micro", "@cf/meta/llama-3.2-3b-instruct",
            "@cf/meta/llama-3.1-8b-instruct-fp8-fast", "@cf/meta/llama-3.2-11b-vision-instruct",
            "@cf/google/gemma-4-26b-a4b-it", "@cf/qwen/qwen3-30b-a3b-fp8",
            "@cf/mistralai/mistral-small-3.1-24b-instruct", "@cf/openai/gpt-oss-20b",
            "@cf/meta/llama-4-scout-17b-16e-instruct", "@cf/meta/llama-3.3-70b-instruct-fp8-fast",
            "@cf/openai/gpt-oss-120b", "@cf/nvidia/nemotron-3-120b-a12b",
            "@cf/deepseek-ai/deepseek-v4.1-flash-1017", "@cf/moonshotai/kimi-k2.6",
        ];
        assert_eq!(w.iter().map(|m| documented_efforts(m).unwrap().len()).sum::<usize>(), 22);
        // Sol and Astra have no `none`; Opus and Fable cannot turn thinking off.
        for m in ["gpt-6.1-sol", "gpt-6-astra", "claude-opus-5-5", "claude-fable-5-1"] {
            assert!(!documented_efforts(m).unwrap().contains(&Effort::None));
        }
        assert!(excluded_workers_ai("@cf/zai-org/glm-4.7-flash") && !excluded_workers_ai(w[0]));
        for e in [D, N, L, M, H, XH, X] {
            assert_eq!(Effort::parse(e.code()), Some(e));
        }
    }
}
