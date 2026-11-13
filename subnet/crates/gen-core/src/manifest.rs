//! The generator manifest (`slop_ninja_generator_manifest_v1`).
//!
//! A private JSON object in the eval bucket, bound in the plan by `{key,
//! sha256, bytes}`. The reader refuses an object whose length is not the
//! declared one before it parses anything ([`Manifest::parse_bound`]); the
//! caller refuses a larger object before allocating (arena rule). Every
//! change (a model, a price, an effort list, a template, a measured value) is
//! a new `version` and a new digest, and the rotation's streams are keyed by
//! the version, so a new manifest starts fresh decks.
//!
//! `status` is `planned` until the measurement run has recorded every (model,
//! effort) cell; a plan refuses a `planned` manifest ([`crate::plan`]).

use crate::vendor::{DocKind, Effort, Lane, Route, Vendor, documented_efforts, excluded_workers_ai};
use crate::{MANIFEST_SCHEMA, STREAM_BOUND};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// Why a manifest (or a voice library, or a plan field) was refused: a fixed,
/// content-free reason.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Refused(pub &'static str);

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}

fn need(c: bool, why: &'static str) -> Result<(), Refused> {
    if c { Ok(()) } else { Err(Refused(why)) }
}

pub(crate) fn hex32(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// An R2 key the manifest or plan may name: printable ASCII path segments,
/// no `..`, bounded by R2's documented 1,024-byte key limit.
pub(crate) fn key_ok(k: &str) -> bool {
    !k.is_empty()
        && k.len() <= 1024
        && !k.starts_with('/')
        && k.split('/').all(|s| !s.is_empty() && s != "." && s != "..")
        && k.bytes().all(|b| b.is_ascii_graphic())
}

/// A private object bound by digest and length.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ObjectRef {
    pub key: String,
    pub sha256: String,
    pub bytes: u64,
}

impl ObjectRef {
    pub fn validate(&self, what: &'static str) -> Result<(), Refused> {
        need(key_ok(&self.key) && hex32(&self.sha256) && self.bytes > 0, what)
    }
    /// Whether `bytes` is exactly this object (length first, then digest).
    pub fn matches(&self, bytes: &[u8]) -> bool {
        bytes.len() as u64 == self.bytes && crate::hash::sha256_hex(bytes) == self.sha256
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Planned,
    Measured,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Share {
    pub share_num: u32,
    pub share_den: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LaneShares {
    pub frontier: Share,
    pub workers_ai: Share,
}

/// Micro-USD per million tokens.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Price {
    pub input: u64,
    pub output: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_read: Option<u64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Prices {
    pub standard: Price,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flex: Option<Price>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub batch: Option<Price>,
    /// The price at a configured endpoint ([`crate::vendor::Tier::Endpoint`]):
    /// required for a model whose vendor the manifest routes to one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<Price>,
}

/// A documented price condition: prompts above `prompt_tokens_above` are
/// priced at `price` (Haiku's 100K tier, OpenAI's 272K long-context rate).
/// A request that would cross it is priced at the higher rate.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PriceCondition {
    pub prompt_tokens_above: u64,
    pub price: Price,
}

/// The vendor's documented limits for the account's tier (informational; a
/// 429 with `retry-after` remains the authority).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RateLimits {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rpm: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tpm: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub itpm: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub otpm: Option<u64>,
    pub source: String,
}

/// How a Workers AI model with a reasoning setting takes it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WaiReasoning {
    /// `reasoning: {effort: <level>}`.
    ReasoningObject,
    /// `reasoning_effort: <level>`.
    ReasoningEffort,
}

/// The measurement run's figures for one (model, effort) cell.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Measured {
    pub calls: u32,
    pub accepted: u32,
    pub refusals: u32,
    pub input_tokens_max: u64,
    pub output_tokens_max: u64,
    pub reasoning_tokens_max: u64,
    pub latency_ms_max: u64,
    pub first_token_ms_max: u64,
    /// The longest SSE line seen beside its visible text (envelope bytes).
    pub envelope_bytes_max: u32,
    /// The widest validator word window (its maximum) the cell was measured
    /// at: figures for a wider item are scaled up by the ratio
    /// ([`Measured::scaled`]), never assumed.
    pub window_words_max: u32,
}

impl Measured {
    /// `x` scaled to an item whose window maximum is `window_words`: `x`
    /// itself up to the measured window, proportionally above it (rounded
    /// up). Output and latency grow at most linearly with the requested
    /// length; reasoning does not shrink.
    pub fn scaled(&self, x: u64, window_words: u32) -> u64 {
        let w = u64::from(window_words);
        let base = u64::from(self.window_words_max.max(1));
        if w <= base { x } else { u64::try_from((u128::from(x) * u128::from(w)).div_ceil(u128::from(base))).unwrap_or(u64::MAX) }
    }
}

/// Why the measurement run marked a cell unusable (recorded with the cell).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Unusable {
    /// No measured call produced an accepted answer.
    NoAcceptedAnswer,
    /// The cell's slowest measured call is past the longest call timeout any
    /// frontier lease the lane bounds allow can give it.
    SlowerThanLeaseCeiling,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelEntry {
    /// The exact API id (Workers AI: the `@cf/...` id).
    pub id: String,
    pub lane: Lane,
    pub vendor: Vendor,
    pub family: String,
    pub tier: String,
    /// Every level the vendor documents for this model, in order.
    pub effort_levels: Vec<Effort>,
    pub prices: Prices,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub price_conditions: Vec<PriceCondition>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rate_limits: Option<RateLimits>,
    /// The vendor's batch API is allowed for this model.
    pub batch: bool,
    /// The documented maximum output tokens.
    pub max_output_tokens: u32,
    /// Ids the vendor may report for this model (dated aliases). The exact
    /// id is always accepted.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub accepted_aliases: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wai_reasoning: Option<WaiReasoning>,
    /// Per effort level (its code), the measurement run's figures.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub measured: Option<BTreeMap<String, Measured>>,
    /// Levels the measurement run found unusable in this version (recorded,
    /// never dropped silently): the rotation skips them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unusable: Vec<Effort>,
    /// Per unusable level (its code), why.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub unusable_reasons: BTreeMap<String, Unusable>,
}

impl ModelEntry {
    /// The levels the rotation spreads over (documented, not unusable).
    pub fn usable_efforts(&self) -> Vec<Effort> {
        self.effort_levels.iter().copied().filter(|e| !self.unusable.contains(e)).collect()
    }
    pub fn measured(&self, e: Effort) -> Option<&Measured> {
        self.measured.as_ref()?.get(e.code())
    }
    pub fn price(&self, tier: crate::vendor::Tier) -> Option<Price> {
        use crate::vendor::Tier;
        match tier {
            Tier::Standard => Some(self.prices.standard),
            Tier::Flex => self.prices.flex,
            Tier::Batch => self.prices.batch,
            Tier::Endpoint => self.prices.endpoint,
        }
    }
    /// Whether a reported model id names this model.
    pub fn reports_as(&self, reported: &str) -> bool {
        reported == self.id || self.accepted_aliases.iter().any(|a| a == reported)
    }
}

/// The templates a manifest must define.
pub const TEMPLATE_IDS: [&str; 5] =
    ["rewrite_v2_plain", "rewrite_v2_voiced", "rewrite_v2_match_context", "scratch_v2_plain", "scratch_v2_voiced"];

/// The placeholders a template may use (`{name}`).
pub const PLACEHOLDERS: [&str; 9] = [
    "voice",
    "operation",
    "register",
    "n_words",
    "passage",
    "context_before",
    "context_after",
    "topic",
    "target_words",
];

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Template {
    pub id: String,
    pub kind: DocKind,
    pub system: String,
    pub user: String,
}

/// The `{name}` placeholders of `t`, or `None` when a brace is unbalanced.
pub fn placeholders(t: &str) -> Option<Vec<&str>> {
    let mut out = Vec::new();
    let mut rest = t;
    while let Some(open) = rest.find('{') {
        let after = rest.get(open + 1..)?;
        let close = after.find('}')?;
        let name = after.get(..close)?;
        if name.contains('{') {
            return None;
        }
        out.push(name);
        rest = after.get(close + 1..)?;
    }
    if rest.contains('}') {
        return None;
    }
    Some(out)
}

impl Template {
    fn validate(&self) -> Result<(), Refused> {
        let all: Vec<&str> = match (placeholders(&self.system), placeholders(&self.user)) {
            (Some(a), Some(b)) => a.into_iter().chain(b).collect(),
            _ => return Err(Refused("templates (unbalanced braces)")),
        };
        need(all.iter().all(|p| PLACEHOLDERS.contains(p)), "templates (unknown placeholder)")?;
        let has = |p: &str| all.contains(&p);
        let sys_voice = placeholders(&self.system).is_some_and(|v| v.contains(&"voice"));
        let voiced = self.id.ends_with("_voiced") || self.id.ends_with("_match_context");
        need(voiced == sys_voice && (voiced || !has("voice")), "templates (voice placeholder: voiced templates only, in the system prompt)")?;
        match self.kind {
            DocKind::Mixed => {
                need(self.id.starts_with("rewrite_v2_"), "templates (kind)")?;
                need(has("passage") && has("operation") && has("n_words"), "templates (a rewrite names its passage, operation and length)")?;
                need(!has("topic") && !has("target_words"), "templates (a rewrite has no topic)")?;
                let ctx = self.id == "rewrite_v2_match_context";
                need(ctx == (has("context_before") && has("context_after")), "templates (context only in the match-context template)")?;
                need(ctx || (!has("context_before") && !has("context_after")), "templates (context only in the match-context template)")?;
            }
            DocKind::ModelOnly => {
                need(self.id.starts_with("scratch_v2_"), "templates (kind)")?;
                need(has("topic") && has("target_words") && has("register"), "templates (a scratch names its register, topic and length)")?;
                need(!has("passage") && !has("context_before") && !has("context_after") && !has("operation"), "templates (no source text in a scratch prompt)")?;
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Operation {
    pub id: String,
    pub text: String,
}

/// The edit-intensity classes: each holds the operations of its class.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Operations {
    pub light: Vec<Operation>,
    pub heavy: Vec<Operation>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Intensity {
    Light,
    Heavy,
}

impl Intensity {
    pub const ALL: [Intensity; 2] = [Intensity::Light, Intensity::Heavy];
    pub fn code(self) -> &'static str {
        match self {
            Intensity::Light => "light",
            Intensity::Heavy => "heavy",
        }
    }
}

impl Operations {
    pub fn of(&self, i: Intensity) -> &[Operation] {
        match i {
            Intensity::Light => &self.light,
            Intensity::Heavy => &self.heavy,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VoiceLibraryRef {
    pub key: String,
    pub sha256: String,
    pub bytes: u64,
    pub version: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Register {
    pub domain: String,
    pub register: String,
    pub topics: Vec<String>,
}

/// `stream_bound_v1`: the visible text of a call is at most `W_max x
/// (word_bytes_max + 2)` bytes, `W_max` the validator's own upper word window
/// for the item; one SSE line is at most that plus `envelope_bytes` (the
/// measured envelope, versioned here). The same envelope bounds an error body.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StreamBound {
    pub schema: String,
    /// The pinned scorer tokenizer's per-word character limit times 4 (UTF-8).
    pub word_bytes_max: u32,
    pub envelope_bytes: u32,
    /// Whether `envelope_bytes` comes from the measurement run.
    pub envelope_measured: bool,
}

/// Each frontier vendor's route ([`Route`]). Absent (both direct) in a
/// manifest written before routes existed, and then not serialized, so such
/// a manifest's bytes and digest are unchanged.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Routes {
    #[serde(default, skip_serializing_if = "Route::is_direct")]
    pub openai: Route,
    #[serde(default, skip_serializing_if = "Route::is_direct")]
    pub anthropic: Route,
    /// A leading system block that a configured Anthropic endpoint requires,
    /// sent as its own first text block before the template's system prompt
    /// (absent: the system prompt alone, as on the direct route).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anthropic_system_prefix: Option<String>,
}

impl Routes {
    pub fn is_direct(&self) -> bool {
        *self == Routes::default()
    }
    /// The route of `v` (Workers AI runs in the eval Worker: always direct).
    pub fn of(&self, v: Vendor) -> Route {
        match v {
            Vendor::Openai => self.openai,
            Vendor::Anthropic => self.anthropic,
            Vendor::WorkersAi => Route::Direct,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub schema: String,
    pub version: u32,
    pub status: Status,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub measurement_report_sha256: Option<String>,
    /// When the prices were read (content-free provenance).
    pub prices_read: String,
    pub price_sources: Vec<String>,
    pub lanes: LaneShares,
    /// The pinned `anthropic-version` header.
    pub anthropic_version: String,
    /// How each frontier vendor is reached; a new route is a new manifest
    /// version (its request shape and pricing differ, and the measurement
    /// run measures it).
    #[serde(default, skip_serializing_if = "Routes::is_direct")]
    pub routes: Routes,
    pub models: Vec<ModelEntry>,
    pub templates: Vec<Template>,
    pub operations: Operations,
    pub voice_library: VoiceLibraryRef,
    pub span_classes_percent: Vec<u32>,
    pub scratch_words: Vec<u32>,
    pub registers: Vec<Register>,
    pub stream_bound: StreamBound,
}

impl Manifest {
    /// Parses and validates exactly the bound object: a length or digest
    /// other than the binding's is refused before parsing.
    pub fn parse_bound(bytes: &[u8], binding: &ObjectRef) -> Result<Self, Refused> {
        need(binding.matches(bytes), "manifest (length or digest is not the plan's)")?;
        Self::parse(bytes)
    }

    pub fn parse(bytes: &[u8]) -> Result<Self, Refused> {
        let m: Self = serde_json::from_slice(bytes).map_err(|_| Refused("manifest json"))?;
        m.validate()?;
        Ok(m)
    }

    pub fn model(&self, id: &str) -> Option<&ModelEntry> {
        self.models.iter().find(|m| m.id == id)
    }

    /// The lane's models, in manifest order (the rotation's indices).
    pub fn lane_models(&self, lane: Lane) -> Vec<&ModelEntry> {
        self.models.iter().filter(|m| m.lane == lane).collect()
    }

    pub fn register(&self, domain: &str) -> Option<&Register> {
        self.registers.iter().find(|r| r.domain == domain)
    }

    pub fn template(&self, id: &str) -> Option<&Template> {
        self.templates.iter().find(|t| t.id == id)
    }

    /// Whether `reported` is any id or alias of the manifest (a reported id
    /// outside this set is recorded only as "other").
    pub fn known_model(&self, reported: &str) -> bool {
        self.models.iter().any(|m| m.reports_as(reported))
    }

    /// The secondary cells per kind: mixed = span classes x 2 intensities,
    /// model-only = scratch lengths.
    pub fn secondary_cells(&self, kind: DocKind) -> usize {
        match kind {
            DocKind::Mixed => self.span_classes_percent.len() * Intensity::ALL.len(),
            DocKind::ModelOnly => self.scratch_words.len(),
        }
    }

    pub fn validate(&self) -> Result<(), Refused> {
        need(self.schema == MANIFEST_SCHEMA, "manifest schema")?;
        need(self.version >= 1, "manifest version")?;
        need(!self.prices_read.is_empty() && !self.price_sources.is_empty(), "manifest price provenance")?;
        // This build's lane deck is two long: exactly half each.
        let half = Share { share_num: 1, share_den: 2 };
        need(self.lanes.frontier == half && self.lanes.workers_ai == half, "manifest lanes (half frontier, half Workers AI)")?;
        let mut ids = BTreeSet::new();
        for m in &self.models {
            need(ids.insert(m.id.as_str()), "manifest models (distinct ids)")?;
            need(m.vendor.lane() == m.lane, "manifest models (vendor and lane)")?;
            need(!m.family.is_empty() && !m.tier.is_empty(), "manifest models (family and tier)")?;
            need(m.max_output_tokens > 0, "manifest models (max_output_tokens)")?;
            need(m.accepted_aliases.iter().all(|a| !a.is_empty() && a.len() <= 128), "manifest models (aliases)")?;
            match m.vendor {
                Vendor::WorkersAi => {
                    need(m.id.starts_with("@cf/") && m.id.len() <= 96, "manifest models (Workers AI ids are @cf ids)")?;
                    need(!excluded_workers_ai(&m.id), "manifest models (a deprecated or excluded Workers AI id)")?;
                    let documented = documented_efforts(&m.id).unwrap_or(&[Effort::Default]);
                    need(m.effort_levels == documented, "manifest models (effort levels are the documented set)")?;
                    need((m.effort_levels.len() > 1) == m.wai_reasoning.is_some(), "manifest models (a Workers AI reasoning setting exactly when it has levels)")?;
                }
                Vendor::Openai | Vendor::Anthropic => {
                    let documented = documented_efforts(&m.id).ok_or(Refused("manifest models (a frontier model this build has no effort record of)"))?;
                    need(m.effort_levels == documented, "manifest models (effort levels are the documented set)")?;
                    need(m.wai_reasoning.is_none(), "manifest models (wai_reasoning on a frontier model)")?;
                    need(m.vendor != Vendor::Anthropic || crate::vendor::anthropic_thinking(&m.id).is_some(), "manifest models (an Anthropic model without a thinking rule)")?;
                }
            }
            // Direct OpenAI calls run on Flex (Batch prices, synchronously);
            // a configured endpoint has no Flex, and its own price.
            need(m.vendor != Vendor::Openai || !self.routes.openai.is_direct() || m.prices.flex.is_some(), "manifest models (OpenAI flex prices)")?;
            need(self.routes.of(m.vendor).is_direct() || m.prices.endpoint.is_some(), "manifest models (endpoint prices)")?;
            need(!m.batch || m.prices.batch.is_some(), "manifest models (batch prices)")?;
            need(m.unusable.iter().all(|e| m.effort_levels.contains(e)) && !m.usable_efforts().is_empty(), "manifest models (unusable levels)")?;
            need(
                m.unusable_reasons.len() == m.unusable.len() && m.unusable.iter().all(|e| m.unusable_reasons.contains_key(e.code())),
                "manifest models (a reason for every unusable level)",
            )?;
            if let Some(ms) = &m.measured {
                need(ms.keys().all(|k| Effort::parse(k).is_some_and(|e| m.effort_levels.contains(&e))), "manifest models (measured levels)")?;
            }
        }
        for lane in Lane::ALL {
            need(!self.lane_models(lane).is_empty(), "manifest models (both lanes)")?;
        }
        need(self.models.iter().all(|m| m.vendor != Vendor::Anthropic) || !self.anthropic_version.is_empty(), "manifest anthropic_version")?;
        need(
            self.routes.anthropic_system_prefix.as_deref().is_none_or(|s| !self.routes.anthropic.is_direct() && !s.trim().is_empty()),
            "manifest routes (a system prefix only for a configured Anthropic endpoint, and not empty)",
        )?;
        if self.status == Status::Measured {
            need(self.measurement_report_sha256.as_deref().is_some_and(hex32), "manifest measurement_report_sha256")?;
            need(
                self.models.iter().all(|m| m.effort_levels.iter().all(|e| m.measured(*e).is_some_and(|x| x.calls > 0))),
                "manifest measured (every model and effort level)",
            )?;
            need(self.stream_bound.envelope_measured, "manifest stream_bound (envelope measured)")?;
        }
        let mut tids = BTreeSet::new();
        for t in &self.templates {
            need(tids.insert(t.id.as_str()), "templates (distinct ids)")?;
            need(TEMPLATE_IDS.contains(&t.id.as_str()), "templates (unknown id)")?;
            t.validate()?;
        }
        need(tids.len() == TEMPLATE_IDS.len(), "templates (every template)")?;
        for i in Intensity::ALL {
            let ops = self.operations.of(i);
            let distinct: BTreeSet<&str> = ops.iter().map(|o| o.id.as_str()).collect();
            need(!ops.is_empty() && distinct.len() == ops.len() && ops.iter().all(|o| !o.text.is_empty()), "operations")?;
        }
        let light: BTreeSet<&str> = self.operations.light.iter().map(|o| o.id.as_str()).collect();
        need(self.operations.heavy.iter().all(|o| !light.contains(o.id.as_str())), "operations (an operation in both classes)")?;
        let spans: BTreeSet<u32> = self.span_classes_percent.iter().copied().collect();
        need(!spans.is_empty() && spans.len() == self.span_classes_percent.len() && spans.iter().all(|p| (1..100).contains(p)), "span_classes_percent")?;
        let words: BTreeSet<u32> = self.scratch_words.iter().copied().collect();
        need(!words.is_empty() && words.len() == self.scratch_words.len() && words.iter().all(|w| *w > 0), "scratch_words")?;
        let mut doms = BTreeSet::new();
        for r in &self.registers {
            need(doms.insert(r.domain.as_str()) && !r.domain.is_empty(), "registers (distinct domains)")?;
            need(!r.register.is_empty() && !r.topics.is_empty() && r.topics.iter().all(|t| !t.is_empty()), "registers (register and topics)")?;
        }
        need(
            key_ok(&self.voice_library.key) && hex32(&self.voice_library.sha256) && self.voice_library.bytes > 0 && self.voice_library.version >= 1,
            "voice_library",
        )?;
        need(
            self.stream_bound.schema == STREAM_BOUND
                && (1..=crate::iface::WORD_BYTES as u32).contains(&self.stream_bound.word_bytes_max)
                && self.stream_bound.envelope_bytes > 0
                && self.stream_bound.envelope_bytes as usize <= crate::iface::VISIBLE_BYTES_MAX,
            "stream_bound (word bytes within the tokenizer's word limit, envelope within the visible bound)",
        )?;
        Ok(())
    }
}

/// A synthetic manifest over the real roster (vendor facts: ids, effort
/// levels, list prices read 2026-11-04) with placeholder template wording,
/// for tests here and in the Workers' crates. Its voice library binding is a
/// placeholder; pass the library's own reference to `with_voice_library`.
#[cfg(any(test, feature = "fixtures"))]
pub mod fixture {
    use super::*;
    use crate::vendor::{Effort, Lane, Vendor};

    fn p(input: u64, output: u64) -> Price {
        Price { input, output, cache_read: None }
    }

    fn model(id: &str, vendor: Vendor, tier: &str, standard: Price, half: Option<Price>) -> ModelEntry {
        let levels = crate::vendor::documented_efforts(id).unwrap_or(&[Effort::Default]).to_vec();
        let frontier = vendor != Vendor::WorkersAi;
        ModelEntry {
            id: id.into(),
            lane: vendor.lane(),
            vendor,
            family: id.split('/').next_back().unwrap_or(id).split('-').take(2).collect::<Vec<_>>().join("-"),
            tier: tier.into(),
            wai_reasoning: (!frontier && levels.len() > 1).then_some(WaiReasoning::ReasoningObject),
            effort_levels: levels,
            prices: Prices {
                standard,
                flex: if vendor == Vendor::Openai { half } else { None },
                batch: if frontier { half } else { None },
                endpoint: None,
            },
            price_conditions: Vec::new(),
            rate_limits: None,
            batch: frontier,
            max_output_tokens: if frontier { 128_000 } else { 4_096 },
            accepted_aliases: Vec::new(),
            measured: None,
            unusable: Vec::new(),
            unusable_reasons: BTreeMap::new(),
        }
    }

    /// The 22-model roster of the design (section 2.4).
    pub fn roster() -> Vec<ModelEntry> {
        use Vendor::{Anthropic as A, Openai as O, WorkersAi as W};
        let mut haiku = model("claude-haiku-5-5", A, "small", p(100_000, 500_000), Some(p(50_000, 250_000)));
        haiku.price_conditions = vec![PriceCondition { prompt_tokens_above: 100_000, price: p(500_000, 2_500_000) }];
        vec![
            model("gpt-6-luna", O, "small", p(100_000, 500_000), Some(p(50_000, 250_000))),
            model("gpt-6.1-terra", O, "mid-small", p(1_000_000, 5_000_000), Some(p(500_000, 2_500_000))),
            model("gpt-6.1-sol", O, "mid", p(2_000_000, 10_000_000), Some(p(1_000_000, 5_000_000))),
            model("gpt-6-astra", O, "flagship", p(10_000_000, 50_000_000), Some(p(5_000_000, 25_000_000))),
            haiku,
            model("claude-sonnet-5-5", A, "mid", p(2_000_000, 10_000_000), Some(p(1_000_000, 5_000_000))),
            model("claude-opus-5-5", A, "flagship", p(4_000_000, 20_000_000), Some(p(2_000_000, 10_000_000))),
            model("claude-fable-5-1", A, "apex", p(10_000_000, 50_000_000), Some(p(5_000_000, 25_000_000))),
            model("@cf/ibm-granite/granite-4.0-h-micro", W, "tiny", p(17_000, 112_000), None),
            model("@cf/meta/llama-3.2-3b-instruct", W, "tiny", p(51_000, 335_000), None),
            model("@cf/meta/llama-3.1-8b-instruct-fp8-fast", W, "small", p(45_000, 384_000), None),
            model("@cf/meta/llama-3.2-11b-vision-instruct", W, "small", p(49_000, 676_000), None),
            model("@cf/google/gemma-4-26b-a4b-it", W, "mid", p(100_000, 300_000), None),
            model("@cf/qwen/qwen3-30b-a3b-fp8", W, "mid", p(51_000, 335_000), None),
            model("@cf/mistralai/mistral-small-3.1-24b-instruct", W, "mid", p(351_000, 555_000), None),
            model("@cf/openai/gpt-oss-20b", W, "mid", p(200_000, 300_000), None),
            model("@cf/meta/llama-4-scout-17b-16e-instruct", W, "mid-large", p(270_000, 850_000), None),
            model("@cf/meta/llama-3.3-70b-instruct-fp8-fast", W, "large", p(293_000, 2_253_000), None),
            model("@cf/openai/gpt-oss-120b", W, "large", p(350_000, 750_000), None),
            model("@cf/nvidia/nemotron-3-120b-a12b", W, "large", p(500_000, 1_500_000), None),
            model("@cf/deepseek-ai/deepseek-v4.1-flash-1017", W, "frontier-open", p(300_000, 1_200_000), None),
            model("@cf/moonshotai/kimi-k2.6", W, "frontier-open", p(950_000, 4_000_000), None),
        ]
    }

    fn t(id: &str, kind: DocKind, system: &str, user: &str) -> Template {
        Template { id: id.into(), kind, system: system.into(), user: user.into() }
    }

    fn op(id: &str) -> Operation {
        Operation { id: id.into(), text: format!("TEST operation {id}.") }
    }

    /// A planned manifest over [`roster`] with synthetic templates.
    pub fn planned() -> Manifest {
        Manifest {
            schema: MANIFEST_SCHEMA.into(),
            version: 1,
            status: Status::Planned,
            measurement_report_sha256: None,
            prices_read: "2026-11-04".into(),
            price_sources: vec!["vendor pricing pages".into()],
            lanes: LaneShares { frontier: Share { share_num: 1, share_den: 2 }, workers_ai: Share { share_num: 1, share_den: 2 } },
            anthropic_version: "2026-10-21".into(),
            routes: Routes::default(),
            models: roster(),
            templates: vec![
                t("rewrite_v2_plain", DocKind::Mixed, "TEST plain. {operation} Reply with the passage only.", "From {register}. About {n_words} words.\n\n{passage}"),
                t("rewrite_v2_voiced", DocKind::Mixed, "{voice} TEST voiced. {operation} Reply with the passage only.", "From {register}. About {n_words} words.\n\n{passage}"),
                t(
                    "rewrite_v2_match_context",
                    DocKind::Mixed,
                    "{voice} TEST match. {operation} Reply with the replacement passage only.",
                    "From {register}. Before:\n{context_before}\nAfter:\n{context_after}\nRewrite in about {n_words} words:\n{passage}",
                ),
                t("scratch_v2_plain", DocKind::ModelOnly, "TEST writer. Reply with the text only.", "Write {register} about {topic}, about {target_words} words."),
                t("scratch_v2_voiced", DocKind::ModelOnly, "{voice} TEST writer. Reply with the text only.", "Write {register} about {topic}, about {target_words} words."),
            ],
            operations: Operations {
                light: vec![op("smooth"), op("tighten"), op("voice_shift")],
                heavy: vec![op("paraphrase"), op("vocabulary"), op("restructure")],
            },
            voice_library: VoiceLibraryRef { key: "generation/voices/test.jsonl".into(), sha256: "0".repeat(64), bytes: 1, version: 1 },
            span_classes_percent: vec![10, 40, 80],
            scratch_words: vec![150, 200, 250, 300],
            registers: vec![
                Register { domain: "wikipedia".into(), register: "a TEST encyclopedia lead".into(), topics: vec!["a river".into(), "a castle".into()] },
                Register { domain: "fineweb".into(), register: "a TEST blog post".into(), topics: vec!["a bicycle".into()] },
            ],
            stream_bound: StreamBound { schema: STREAM_BOUND.into(), word_bytes_max: 400, envelope_bytes: 4096, envelope_measured: false },
        }
    }

    /// `m` with every cell measured (synthetic figures).
    pub fn measured(mut m: Manifest) -> Manifest {
        for e in &mut m.models {
            e.measured = Some(
                e.effort_levels
                    .iter()
                    .map(|l| {
                        let x = Measured {
                            calls: 20,
                            accepted: 19,
                            refusals: 1,
                            input_tokens_max: 900,
                            output_tokens_max: 6_000,
                            reasoning_tokens_max: 5_000,
                            latency_ms_max: 60_000,
                            first_token_ms_max: 9_000,
                            envelope_bytes_max: 1_200,
                            window_words_max: 2_000,
                        };
                        (l.code().to_owned(), x)
                    })
                    .collect(),
            );
        }
        m.status = Status::Measured;
        m.measurement_report_sha256 = Some("ab".repeat(32));
        m.stream_bound.envelope_measured = true;
        m
    }

    /// The fixture's leading system block for a configured Anthropic endpoint.
    pub const SYSTEM_PREFIX: &str = "TEST endpoint system prefix.";

    /// `m` with both frontier vendors at a configured endpoint (with
    /// [`SYSTEM_PREFIX`] for Anthropic), each frontier model priced there at
    /// its standard price.
    pub fn configured(mut m: Manifest) -> Manifest {
        m.routes = Routes { openai: Route::ConfiguredEndpointV1, anthropic: Route::ConfiguredEndpointV1, anthropic_system_prefix: Some(SYSTEM_PREFIX.into()) };
        for e in m.models.iter_mut().filter(|e| e.vendor != Vendor::WorkersAi) {
            e.prices.endpoint = Some(e.prices.standard);
        }
        m
    }

    pub fn with_voice_library(mut m: Manifest, r: VoiceLibraryRef) -> Manifest {
        m.voice_library = r;
        m
    }

    /// The ids of a lane's models, in manifest order.
    pub fn lane_ids(m: &Manifest, lane: Lane) -> Vec<String> {
        m.lane_models(lane).iter().map(|x| x.id.clone()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::fixture::*;
    use super::*;

    fn bytes(m: &Manifest) -> Vec<u8> {
        serde_json::to_vec(m).unwrap()
    }

    #[test]
    fn the_fixture_validates_and_round_trips() {
        let m = planned();
        m.validate().unwrap();
        assert_eq!(m.models.len(), 22);
        assert_eq!(m.lane_models(Lane::Frontier).len(), 8);
        assert_eq!(m.lane_models(Lane::WorkersAi).len(), 14);
        let cells: usize = m.models.iter().map(|x| x.effort_levels.len()).sum();
        assert_eq!(cells, 66);
        let b = bytes(&m);
        assert_eq!(Manifest::parse(&b).unwrap(), m);
        measured(m).validate().unwrap();
    }

    #[test]
    fn the_binding_is_checked_before_parsing() {
        let b = bytes(&planned());
        let r = ObjectRef { key: "generation/manifests/generator-manifest-v1.json".into(), sha256: crate::hash::sha256_hex(&b), bytes: b.len() as u64 };
        assert!(Manifest::parse_bound(&b, &r).is_ok());
        let short = ObjectRef { bytes: r.bytes - 1, ..r.clone() };
        assert_eq!(Manifest::parse_bound(&b, &short).unwrap_err().0, "manifest (length or digest is not the plan's)");
        let mut other = b.clone();
        other[10] ^= 1;
        assert!(Manifest::parse_bound(&other, &r).is_err());
    }

    #[test]
    fn unknown_fields_and_wrong_effort_sets_are_refused() {
        let mut v: serde_json::Value = serde_json::to_value(planned()).unwrap();
        v["extra"] = 1.into();
        assert_eq!(Manifest::parse(&serde_json::to_vec(&v).unwrap()).unwrap_err().0, "manifest json");
        let mut v: serde_json::Value = serde_json::to_value(planned()).unwrap();
        v["models"][0]["surprise"] = true.into();
        assert!(Manifest::parse(&serde_json::to_vec(&v).unwrap()).is_err());
        // Sol has no `none`; adding it is refused, as is dropping `max` from Luna.
        let mut m = planned();
        m.models.iter_mut().find(|x| x.id == "gpt-6.1-sol").unwrap().effort_levels.insert(0, Effort::None);
        assert_eq!(m.validate().unwrap_err().0, "manifest models (effort levels are the documented set)");
        let mut m = planned();
        m.models[0].effort_levels.pop();
        assert!(m.validate().is_err());
        // Deprecated and GLM ids never enter.
        let mut m = planned();
        m.models.last_mut().unwrap().id = "@cf/mistral/mistral-7b-instruct-v0.1".into();
        m.models.last_mut().unwrap().effort_levels = vec![Effort::Default];
        m.models.last_mut().unwrap().wai_reasoning = None;
        assert_eq!(m.validate().unwrap_err().0, "manifest models (a deprecated or excluded Workers AI id)");
        let mut m = planned();
        m.models[1].id = m.models[0].id.clone();
        assert_eq!(m.validate().unwrap_err().0, "manifest models (distinct ids)");
        let mut m = planned();
        m.models[0].prices.flex = None;
        assert_eq!(m.validate().unwrap_err().0, "manifest models (OpenAI flex prices)");
    }

    #[test]
    fn fable_5_2_validates_low_to_max_and_refuses_none() {
        // Claude Fable 5.2: effort low to max, thinking always on, so no
        // `none` level. Not in the fixture roster; added here as its own entry.
        let mut m = planned();
        let mut f = m.model("claude-fable-5-1").unwrap().clone();
        f.id = "claude-fable-5-2".into();
        m.models.push(f);
        m.validate().unwrap();
        assert_eq!(m.model("claude-fable-5-2").unwrap().effort_levels, [Effort::Low, Effort::Medium, Effort::High, Effort::Xhigh, Effort::Max]);
        configured(m.clone()).validate().unwrap();
        measured(m.clone()).validate().unwrap();
        let mut none = m.clone();
        none.models.last_mut().unwrap().effort_levels.insert(0, Effort::None);
        assert_eq!(none.validate().unwrap_err().0, "manifest models (effort levels are the documented set)");
        let mut short = m;
        short.models.last_mut().unwrap().effort_levels.pop();
        assert_eq!(short.validate().unwrap_err().0, "manifest models (effort levels are the documented set)");
    }

    #[test]
    fn a_measured_manifest_needs_every_cell() {
        let mut m = measured(planned());
        m.models[3].measured.as_mut().unwrap().remove("max");
        assert_eq!(m.validate().unwrap_err().0, "manifest measured (every model and effort level)");
        let mut m = planned();
        m.status = Status::Measured;
        assert!(m.validate().is_err());
    }

    #[test]
    fn templates_are_checked() {
        let mut m = planned();
        m.templates[0].system.push_str(" {voice}");
        assert!(m.validate().is_err());
        let mut m = planned();
        m.templates[3].user.push_str(" {passage}");
        assert_eq!(m.validate().unwrap_err().0, "templates (no source text in a scratch prompt)");
        let mut m = planned();
        m.templates[1].user.push_str(" {context_before}");
        assert!(m.validate().is_err());
        let mut m = planned();
        m.templates[2].user.push_str(" {secret}");
        assert_eq!(m.validate().unwrap_err().0, "templates (unknown placeholder)");
        let mut m = planned();
        m.templates.pop();
        assert_eq!(m.validate().unwrap_err().0, "templates (every template)");
        assert_eq!(placeholders("a {b} c {d}"), Some(vec!["b", "d"]));
        assert_eq!(placeholders("a {b c"), None);
        assert_eq!(placeholders("a } b"), None);
    }

    #[test]
    fn routes_are_absent_for_direct_manifests_and_lift_only_the_flex_price_rule() {
        let m = planned();
        // A direct manifest serializes as before routes existed (same digest).
        assert!(!String::from_utf8(bytes(&m)).unwrap().contains("routes"));
        assert!(!String::from_utf8(bytes(&m)).unwrap().contains("endpoint"));
        assert_eq!(Manifest::parse(&bytes(&m)).unwrap().routes, Routes::default());
        let p = configured(planned());
        let j: serde_json::Value = serde_json::from_slice(&bytes(&p)).unwrap();
        assert_eq!(
            j["routes"],
            serde_json::json!({"openai": "configured_endpoint_v1", "anthropic": "configured_endpoint_v1", "anthropic_system_prefix": SYSTEM_PREFIX})
        );
        assert_eq!(Manifest::parse(&bytes(&p)).unwrap(), p);
        // Direct OpenAI needs Flex prices; a configured endpoint has no Flex.
        let mut d = planned();
        for e in d.models.iter_mut().filter(|e| e.vendor == Vendor::Openai) {
            e.prices.flex = None;
        }
        assert_eq!(d.validate(), Err(Refused("manifest models (OpenAI flex prices)")));
        d.routes.openai = Route::ConfiguredEndpointV1;
        // A configured endpoint needs its price for every model it serves.
        assert_eq!(d.validate(), Err(Refused("manifest models (endpoint prices)")));
        let ep = Price { input: 7, output: 9, cache_read: None };
        for e in d.models.iter_mut().filter(|e| e.vendor == Vendor::Openai) {
            e.prices.endpoint = Some(ep);
        }
        assert_eq!(d.validate(), Ok(()));
        // The endpoint tier is priced from the manifest; the others keep theirs.
        let luna = d.model("gpt-6-luna").unwrap();
        assert_eq!(luna.price(crate::vendor::Tier::Endpoint), Some(ep));
        assert_eq!(luna.price(crate::vendor::Tier::Standard), Some(luna.prices.standard));
        assert_eq!(planned().model("gpt-6-luna").unwrap().price(crate::vendor::Tier::Endpoint), None);
        assert_eq!(p.routes.of(Vendor::WorkersAi), Route::Direct);
        // A system prefix only with a configured Anthropic endpoint, never blank.
        let mut s = d.clone();
        s.routes.anthropic_system_prefix = Some("x".into());
        assert_eq!(s.validate(), Err(Refused("manifest routes (a system prefix only for a configured Anthropic endpoint, and not empty)")));
        let mut s = p.clone();
        s.routes.anthropic_system_prefix = Some(" ".into());
        assert!(s.validate().is_err());
        s.routes.anthropic_system_prefix = None;
        assert_eq!(s.validate(), Ok(()));
        let bad = br#"{"openai":"elsewhere"}"#;
        assert!(serde_json::from_slice::<Routes>(bad).is_err());
    }
}
