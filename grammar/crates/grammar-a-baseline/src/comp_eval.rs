//! Competitive-A evaluator (protocol `docs/a-competitive-protocol.md`).
//!
//! Numeric only: it reads bundle metadata (`rows.jsonl`) and predictions
//! (`predictions.jsonl`) and never reads, prints or inspects any text field.
//! See `docs/a-competitive-evaluator.md` for the input schema and every metric
//! definition.

use crate::pangram_subsample::{
    self as ps, Cell, SHARE_BINS, bootstrap_interval, clopper_pearson_upper, max_flags, stratum_key,
};
use anyhow::{Context, Result, bail, ensure};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::{Mutex, OnceLock};

pub const EVAL_SCHEMA: &str = "slop_ninja_a_comp_evaluation_v1";
/// Splits that are opened once (Dev-select) or sealed (CAL, Test).
pub const SEALED_SPLITS: [&str; 3] = ["dev_select", "cal", "test"];
pub const PRIMARY_OPERATIONS: [&str; 3] = ["mirror", "span", "full"];
pub const ROBUSTNESS_OPERATIONS: [&str; 2] = ["continuation", "splice"];
/// Three-way class order used everywhere: human_only, model_only, mixed.
pub const ORIGINS: [&str; 3] = ["human_only", "model_only", "mixed"];
pub const RELEASED_SEED: u64 = 17;
/// The protocol's slice minimum (reported as a shortfall, never a gate here).
pub const SLICE_MIN_GROUPS: usize = 15;
pub const SLICE_MIN_HUMANS: usize = 150;
/// The pooled `frontier` slice also needs this many frontier positives.
pub const SLICE_MIN_FRONTIER_POSITIVES: usize = 150;
/// Bundle `population` values inside the primary population (protocol
/// "Primary metric": Test humans, their instruct positives and the frontier
/// Test positives). Every other population is reported as its own slice.
pub const PRIMARY_POPULATIONS: [&str; 2] = ["primary", "frontier"];
pub const FRONTIER_POPULATION: &str = "frontier";
/// Populations whose name starts with this are the robustness slice.
pub const ROBUSTNESS_POPULATION_PREFIX: &str = "robustness";
/// The protocol's fixed list of gated domain slices ("Gated slices").
pub const GATED_DOMAINS: [&str; 15] = [
    "project_gutenberg",
    "wikipedia",
    "wikinews",
    "plos",
    "us_federal_register",
    "fineweb",
    "global_voices",
    "fineweb_blogs",
    "reddit",
    "ubuntu_irc",
    "wikipedia_talk",
    "wikisource",
    "instructional",
    "peer_review",
    "legal_opinions",
];
/// Splits whose operating points must be the CAL-declared thresholds.
pub const DECLARED_OPERATING_POINT_SPLITS: [&str; 1] = ["test"];
/// Fallback labels for a contributor outside the model's roster.
pub const OTHER_AI: &str = "other_ai";
pub const OTHER_HUMAN: &str = "other_human";
/// Object keys of `style_axes` that name a style axis (other keys, such as
/// the inventor or voice ladder, are provenance, not axes).
pub const AXIS_KEYS: [&str; 3] = ["axis", "library_axis", "persona_axis"];
const PROB_TOL: f64 = 1e-6;

/// Target human false-positive rate as an exact rational.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Alpha {
    pub num: u64,
    pub den: u64,
    pub label: &'static str,
}
pub const ALPHA_1PCT: Alpha = Alpha {
    num: 1,
    den: 100,
    label: "0.01",
};
pub const ALPHA_01PCT: Alpha = Alpha {
    num: 1,
    den: 1000,
    label: "0.001",
};
pub const ALPHAS: [Alpha; 2] = [ALPHA_1PCT, ALPHA_01PCT];

// ---------------------------------------------------------------- inputs

#[derive(Deserialize)]
struct RawRow {
    id: String,
    split: String,
    #[serde(alias = "origin_class")]
    origin: String,
    #[serde(alias = "measured_share")]
    share: f64,
    #[serde(default, alias = "replacement_spans", alias = "ai_byte_spans")]
    spans: Option<Vec<[u64; 2]>>,
    #[serde(
        default,
        alias = "attribution_label",
        alias = "generator_attribution_label"
    )]
    generator_label: Option<String>,
    #[serde(default)]
    generator_family: Option<String>,
    #[serde(default, alias = "cohort")]
    cohort_id: Option<String>,
    #[serde(default)]
    cohort_weight: Option<f64>,
    #[serde(alias = "group", alias = "source_family")]
    source_group: String,
    domain: String,
    #[serde(default)]
    genre: Option<String>,
    #[serde(default)]
    voice_id: Option<String>,
    #[serde(default, alias = "voice_cluster_id")]
    voice_cluster: Option<String>,
    #[serde(default)]
    voice_origin: Option<String>,
    #[serde(default)]
    style_axes: Option<Value>,
    #[serde(default)]
    operation: Option<String>,
    #[serde(default)]
    held_out_style: Option<bool>,
    /// Authoritative population label written by the assembler.
    population: String,
    #[serde(default)]
    voice_kind: Option<String>,
    #[serde(default)]
    ocr_tier: Option<Value>,
    #[serde(default)]
    removed_markup_per_100_words: Option<f64>,
    #[serde(default)]
    markup_removed: Option<bool>,
    #[serde(default)]
    demographics: Option<Map<String, Value>>,
    /// Named human profile of an enrolled writer (null: none enrolled).
    #[serde(default)]
    human_attribution_label: Option<String>,
}

/// One validated bundle row (metadata only).
#[derive(Clone, Debug)]
pub struct Row {
    pub id: String,
    pub split: String,
    pub cell: Cell,
    /// 0 human_only, 1 model_only, 2 mixed (see [`ORIGINS`]).
    pub origin: usize,
    pub y: f64,
    pub share: f64,
    pub label: Option<String>,
    pub family: Option<String>,
    pub cohort: Option<String>,
    pub group: String,
    pub domain: String,
    pub genre: Option<String>,
    pub voice_id: Option<String>,
    pub voice_cluster: Option<String>,
    pub voice_origin: Option<String>,
    pub axes: Vec<String>,
    pub operation: Option<String>,
    pub held_out_style: bool,
    pub population: String,
    /// `population` is one of [`PRIMARY_POPULATIONS`].
    pub primary: bool,
    pub voice_kind: Option<String>,
    pub ocr_tier: Option<String>,
    /// Whether transcription markup was removed (from `markup_removed` or a
    /// positive `removed_markup_per_100_words`).
    pub markup_removed: Option<bool>,
    pub demographics: Vec<(String, String)>,
    pub human_label: Option<String>,
}

impl Row {
    pub fn is_human(&self) -> bool {
        self.origin == 0
    }
    pub fn is_frontier(&self) -> bool {
        self.population == FRONTIER_POPULATION
    }
    pub fn is_robustness(&self) -> bool {
        self.population.starts_with(ROBUSTNESS_POPULATION_PREFIX)
    }
    /// Pangram-subsample stratum key `domain|cell[:bin]`.
    pub fn stratum(&self) -> Result<String> {
        stratum_key(
            &self.domain,
            self.cell,
            (self.cell == Cell::Mixed).then_some(self.share),
        )
    }
}

fn opt_str(v: Option<String>) -> Option<String> {
    v.filter(|s| !s.is_empty())
}

fn axes(v: Option<Value>) -> Result<Vec<String>> {
    let mut out: Vec<String> = match v {
        None | Some(Value::Null) => vec![],
        Some(Value::String(s)) => vec![s],
        Some(Value::Array(a)) => a
            .into_iter()
            .map(|x| match x {
                Value::String(s) => Ok(s),
                _ => bail!("style_axes entries must be strings"),
            })
            .collect::<Result<_>>()?,
        // An object (assembler rows) names its axis under one of AXIS_KEYS;
        // its other keys are provenance.
        Some(Value::Object(m)) => AXIS_KEYS
            .iter()
            .filter_map(|k| m.get(*k))
            .map(|v| match v {
                Value::Null => Ok(String::new()),
                Value::String(s) => Ok(s.clone()),
                _ => bail!("style_axes axis values must be strings"),
            })
            .collect::<Result<_>>()?,
        Some(_) => bail!("style_axes must be null, a string, an array or an object"),
    };
    out.retain(|s| !s.is_empty());
    out.sort();
    out.dedup();
    Ok(out)
}

impl Row {
    fn from_raw(r: RawRow) -> Result<Self> {
        let id = r.id;
        ensure!(!id.is_empty(), "empty id");
        let cell = Cell::parse(&r.origin).with_context(|| format!("row {id}"))?;
        let origin = match cell {
            Cell::HumanOnly => 0,
            Cell::ModelOnly => 1,
            Cell::Mixed => 2,
        };
        ensure!(r.share.is_finite(), "row {id}: non-finite share");
        match cell {
            Cell::HumanOnly => ensure!(r.share == 0.0, "row {id}: human_only share must be 0"),
            Cell::ModelOnly => ensure!(r.share == 1.0, "row {id}: model_only share must be 1"),
            Cell::Mixed => ensure!(
                r.share > 0.0 && r.share <= 1.0,
                "row {id}: mixed share must be in (0, 1]"
            ),
        }
        let label = opt_str(r.generator_label);
        ensure!(
            (cell == Cell::HumanOnly) == label.is_none(),
            "row {id}: generator label must be null exactly for human_only"
        );
        if let Some(spans) = &r.spans {
            ensure!(
                cell != Cell::HumanOnly || spans.is_empty(),
                "row {id}: human_only with replacement spans"
            );
            for w in spans.windows(2) {
                ensure!(
                    w[0][1] <= w[1][0],
                    "row {id}: spans unsorted or overlapping"
                );
            }
            ensure!(
                spans.iter().all(|s| s[0] < s[1]),
                "row {id}: empty or reversed span"
            );
        }
        ensure!(!r.source_group.is_empty(), "row {id}: empty source group");
        ensure!(
            !r.domain.is_empty() && !r.domain.contains('|'),
            "row {id}: bad domain"
        );
        ensure!(!r.split.is_empty(), "row {id}: empty split");
        if let Some(w) = r.cohort_weight {
            ensure!(w.is_finite() && w >= 0.0, "row {id}: bad cohort weight");
        }
        // The assembler writes `human` as the operation of human rows; it means none.
        let operation = opt_str(r.operation)
            .filter(|o| o != "none" && !(o == "human" && cell == Cell::HumanOnly));
        match (&operation, cell) {
            (Some(o), Cell::HumanOnly) => bail!("row {id}: human_only with operation {o}"),
            (None, Cell::ModelOnly | Cell::Mixed) => bail!("row {id}: positive without operation"),
            // Primary and robustness populations have closed operation sets (checked
            // below); other supplementary populations carry the generator's own
            // operation labels (`polish`, `condense`, ...), reported as the
            // `operation` slice.
            (Some(o), _) => ensure!(
                o.bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_'),
                "row {id}: malformed operation {o}"
            ),
            _ => {}
        }
        let cohort = opt_str(r.cohort_id);
        let population = r.population;
        ensure!(!population.is_empty(), "row {id}: empty population");
        let primary = PRIMARY_POPULATIONS.contains(&population.as_str());
        if let Some(o) = &operation {
            ensure!(
                !primary || PRIMARY_OPERATIONS.contains(&o.as_str()),
                "row {id}: population {population} with operation {o} (primary positives are mirrors, spans and full rewrites)"
            );
            ensure!(
                !population.starts_with(ROBUSTNESS_POPULATION_PREFIX)
                    || ROBUSTNESS_OPERATIONS.contains(&o.as_str()),
                "row {id}: robustness population with operation {o}"
            );
        }
        ensure!(
            population != FRONTIER_POPULATION || cell != Cell::HumanOnly,
            "row {id}: human_only row in the frontier population"
        );
        let ocr_tier = match r.ocr_tier {
            None | Some(Value::Null) => None,
            Some(Value::String(s)) => opt_str(Some(s)),
            Some(v @ (Value::Number(_) | Value::Bool(_))) => Some(v.to_string()),
            Some(_) => bail!("row {id}: ocr_tier must be a string or number"),
        };
        if let Some(x) = r.removed_markup_per_100_words {
            ensure!(
                x.is_finite() && x >= 0.0,
                "row {id}: bad removed_markup_per_100_words"
            );
        }
        let markup_removed = r
            .markup_removed
            .or(r.removed_markup_per_100_words.map(|x| x > 0.0));
        let mut demographics = vec![];
        for (k, v) in r.demographics.unwrap_or_default() {
            let v = match v {
                Value::Null => continue,
                Value::String(s) => s,
                v @ (Value::Number(_) | Value::Bool(_)) => v.to_string(),
                _ => bail!("row {id}: demographic {k} must be a scalar"),
            };
            demographics.push((k, v));
        }
        let human_label = opt_str(r.human_attribution_label);
        ensure!(
            human_label.is_none() || cell != Cell::ModelOnly,
            "row {id}: model_only row with a human attribution label"
        );
        let family = opt_str(r.generator_family).or_else(|| label.as_deref().map(default_family));
        Ok(Row {
            y: if origin == 0 { 0.0 } else { 1.0 },
            id,
            split: r.split,
            cell,
            origin,
            share: r.share,
            label,
            family,
            cohort,
            group: r.source_group,
            domain: r.domain,
            genre: opt_str(r.genre),
            voice_id: opt_str(r.voice_id),
            voice_cluster: opt_str(r.voice_cluster),
            voice_origin: opt_str(r.voice_origin),
            axes: axes(r.style_axes)?,
            operation,
            held_out_style: r.held_out_style.unwrap_or(false),
            population,
            primary,
            voice_kind: opt_str(r.voice_kind),
            ocr_tier,
            markup_removed,
            demographics,
            human_label,
        })
    }
}

/// Generator family when a row names none: `requested:` is dropped and a
/// hosted model label (`claude-…`, `gpt-…`) maps to its vendor lineage
/// prefix; any other label is its own family (local families use their
/// family id as attribution label).
pub fn default_family(label: &str) -> String {
    let l = label.strip_prefix("requested:").unwrap_or(label);
    for p in ["claude", "gpt"] {
        if l == p || l.starts_with(&format!("{p}-")) {
            return p.to_string();
        }
    }
    l.to_string()
}

/// Parse `rows.jsonl` (all splits) without reading any text field.
pub fn parse_rows(jsonl: &str) -> Result<Vec<Row>> {
    let mut out = Vec::new();
    let mut ids = BTreeSet::new();
    for (i, line) in jsonl.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let raw: RawRow =
            serde_json::from_str(line).with_context(|| format!("rows.jsonl line {}", i + 1))?;
        let row = Row::from_raw(raw).with_context(|| format!("rows.jsonl line {}", i + 1))?;
        ensure!(ids.insert(row.id.clone()), "duplicate row id {}", row.id);
        out.push(row);
    }
    Ok(out)
}

/// One document's model output.
#[derive(Clone, Debug, PartialEq)]
pub struct Prediction {
    pub p_human: f64,
    pub p_model_only: f64,
    pub p_mixed: f64,
    pub share: f64,
    /// Conditional AI attribution scores (label to probability).
    pub attribution: BTreeMap<String, f64>,
    /// Completed role results (`human`, `ai`), when the system returns them.
    pub roles: BTreeMap<String, RoleOutput>,
}

/// One role's completed attribution result: the returned profile (a named
/// profile or `other_<role>`), the role involvement probability and the
/// conditional named scores (the role's roster).
#[derive(Clone, Debug, PartialEq)]
pub struct RoleOutput {
    pub profile: String,
    pub involvement_probability: Option<f64>,
    pub named: BTreeMap<String, f64>,
}

pub const ROLES: [&str; 2] = ["human", "ai"];

fn role_fallback(role: &str) -> &'static str {
    if role == "human" {
        OTHER_HUMAN
    } else {
        OTHER_AI
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Obs {
    Ok(Prediction),
    /// Execution failure: crash, timeout, non-finite output or unsupported input.
    Failed(String),
}

#[derive(Deserialize)]
struct RawPred {
    id: String,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    failure: Option<String>,
    #[serde(default)]
    p_model_only: Option<f64>,
    #[serde(default)]
    p_mixed: Option<f64>,
    #[serde(default)]
    p_human: Option<f64>,
    #[serde(default)]
    share: Option<f64>,
    /// Either a label-to-probability map (AI role) or the shared role form
    /// `{"human": {...}, "ai": {...}}`.
    #[serde(default, alias = "attribution_probabilities", alias = "ai_attribution")]
    attribution: Option<Value>,
    #[serde(default)]
    role_attribution: Option<Value>,
}

enum RoleParse {
    Ok(BTreeMap<String, RoleOutput>),
    NonFinite,
}

/// Parse the role form `{"human": {"profile", "involvement_probability",
/// "conditional_named_scores"}, "ai": {...}}`.
fn parse_roles(v: &Map<String, Value>, id: &str) -> Result<RoleParse> {
    let mut out = BTreeMap::new();
    for (role, x) in v {
        ensure!(
            ROLES.contains(&role.as_str()),
            "prediction {id}: unknown attribution role {role}"
        );
        let o = x
            .as_object()
            .with_context(|| format!("prediction {id}: role {role} must be an object"))?;
        let profile = o
            .get("profile")
            .and_then(Value::as_str)
            .filter(|p| !p.is_empty())
            .with_context(|| format!("prediction {id}: role {role} without profile"))?
            .to_string();
        let involvement_probability = match o.get("involvement_probability") {
            None => None,
            Some(Value::Null) => return Ok(RoleParse::NonFinite),
            Some(p) => Some(unit(
                p.as_f64().context("role involvement probability")?,
                &format!("{role} involvement"),
                id,
            )?),
        };
        let mut named = BTreeMap::new();
        if let Some(m) = o.get("conditional_named_scores").filter(|m| !m.is_null()) {
            for (k, p) in m.as_object().context("conditional_named_scores")? {
                let Some(p) = p.as_f64() else {
                    return Ok(RoleParse::NonFinite);
                };
                named.insert(k.clone(), unit(p, &format!("{role} score {k}"), id)?);
            }
        }
        if !named.is_empty() {
            let t: f64 = named.values().sum();
            ensure!(
                (t - 1.0).abs() <= PROB_TOL,
                "prediction {id}: {role} conditional scores sum to {t}"
            );
        }
        ensure!(
            profile == role_fallback(role) || named.contains_key(&profile),
            "prediction {id}: {role} profile {profile} is neither a roster profile nor {}",
            role_fallback(role)
        );
        out.insert(
            role.clone(),
            RoleOutput {
                profile,
                involvement_probability,
                named,
            },
        );
    }
    Ok(RoleParse::Ok(out))
}

fn unit(x: f64, what: &str, id: &str) -> Result<f64> {
    ensure!(
        (-PROB_TOL..=1.0 + PROB_TOL).contains(&x),
        "prediction {id}: {what} {x} outside [0, 1]"
    );
    Ok(x.clamp(0.0, 1.0))
}

/// Parse `predictions.jsonl`. A row with `status` other than `ok`, or with a
/// null (non-finite) output, is an execution failure; a finite output that
/// breaks the probability contract is an input error.
pub fn parse_predictions(jsonl: &str) -> Result<BTreeMap<String, Obs>> {
    let mut out = BTreeMap::new();
    let mut vocab: Option<BTreeSet<String>> = None;
    for (i, line) in jsonl.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let r: RawPred =
            serde_json::from_str(line).with_context(|| format!("predictions line {}", i + 1))?;
        let id = r.id.clone();
        ensure!(!id.is_empty(), "prediction line {}: empty id", i + 1);
        let obs = (|| -> Result<Obs> {
            if let Some(s) = r.status.as_deref()
                && s != "ok"
            {
                return Ok(Obs::Failed(
                    r.failure.clone().unwrap_or_else(|| s.to_string()),
                ));
            }
            let (Some(h), Some(m), Some(x), Some(s)) =
                (r.p_human, r.p_model_only, r.p_mixed, r.share)
            else {
                return Ok(Obs::Failed("non_finite_or_missing_output".into()));
            };
            let mut attribution = BTreeMap::new();
            let mut roles = BTreeMap::new();
            let role_form = |v: &Value| {
                v.as_object()
                    .is_some_and(|m| !m.is_empty() && m.values().all(Value::is_object))
            };
            let (flat, role_v) = match (&r.attribution, &r.role_attribution) {
                (Some(a), Some(b)) if role_form(a) && !b.is_null() => {
                    bail!("prediction {id}: role attribution given twice")
                }
                (Some(a), b) if role_form(a) => (None, Some(a).or(b.as_ref())),
                (a, b) => (
                    a.as_ref().filter(|v| !v.is_null()),
                    b.as_ref().filter(|v| !v.is_null()),
                ),
            };
            if let Some(rv) = role_v {
                let m = rv.as_object().with_context(|| {
                    format!("prediction {id}: role_attribution must be an object")
                })?;
                match parse_roles(m, &id)? {
                    RoleParse::Ok(x) => roles = x,
                    RoleParse::NonFinite => {
                        return Ok(Obs::Failed("non_finite_attribution".into()));
                    }
                }
            }
            if let Some(f) = flat {
                for (k, v) in f
                    .as_object()
                    .with_context(|| format!("prediction {id}: attribution must be an object"))?
                {
                    let Some(v) = v.as_f64() else {
                        if v.is_null() {
                            return Ok(Obs::Failed("non_finite_attribution".into()));
                        }
                        bail!("prediction {id}: attribution {k} is not a number");
                    };
                    attribution.insert(k.clone(), unit(v, &format!("attribution {k}"), &id)?);
                }
            } else if let Some(ai) = roles.get("ai") {
                // The shared form carries the AI conditional scores in its ai role.
                attribution = ai.named.clone();
            }
            let p = Prediction {
                p_human: unit(h, "p_human", &id)?,
                p_model_only: unit(m, "p_model_only", &id)?,
                p_mixed: unit(x, "p_mixed", &id)?,
                share: unit(s, "share", &id)?,
                attribution,
                roles,
            };
            ensure!(
                (p.p_human + p.p_model_only + p.p_mixed - 1.0).abs() <= PROB_TOL,
                "prediction {id}: origin probabilities do not sum to 1"
            );
            if !p.attribution.is_empty() {
                let t: f64 = p.attribution.values().sum();
                ensure!(
                    (t - 1.0).abs() <= PROB_TOL,
                    "prediction {id}: attribution probabilities sum to {t}"
                );
            }
            Ok(Obs::Ok(p))
        })()?;
        if let Obs::Ok(p) = &obs {
            // Vocabulary signature: AI labels, then each role's roster and presence.
            let mut keys: BTreeSet<String> = p
                .attribution
                .keys()
                .map(|k| format!("ai_scores:{k}"))
                .collect();
            for (role, o) in &p.roles {
                keys.insert(format!("role:{role}"));
                keys.extend(o.named.keys().map(|k| format!("{role}:{k}")));
            }
            if !keys.is_empty() {
                match &vocab {
                    None => vocab = Some(keys),
                    Some(v) => ensure!(
                        *v == keys,
                        "prediction {id}: attribution vocabulary or role set differs from earlier rows"
                    ),
                }
            }
        }
        ensure!(
            out.insert(id.clone(), obs).is_none(),
            "duplicate prediction {id}"
        );
    }
    Ok(out)
}

/// Parse an operating-point value: a number, `"inf"`, or `{"bits": "0x…"}`
/// (optionally with a `value` that must agree).
pub fn parse_threshold(v: &Value) -> Result<f64> {
    let from_bits = |s: &str| -> Result<f64> {
        let h = s.strip_prefix("0x").unwrap_or(s);
        Ok(f64::from_bits(
            u64::from_str_radix(h, 16).context("threshold bits")?,
        ))
    };
    let t = match v {
        Value::Number(n) => n.as_f64().context("threshold")?,
        Value::String(s) if s == "inf" || s == "+inf" || s == "Infinity" => f64::INFINITY,
        Value::String(s) => from_bits(s)?,
        Value::Object(m) => {
            let b = m.get("bits").or_else(|| m.get("f64_bits"));
            let val = m.get("value").filter(|x| !x.is_null());
            match (b, val) {
                (Some(Value::String(b)), val) => {
                    let t = from_bits(b)?;
                    if let Some(val) = val {
                        let x = parse_threshold(val)?;
                        ensure!(
                            x.to_bits() == t.to_bits(),
                            "threshold value and bits differ"
                        );
                    }
                    t
                }
                (None, Some(val)) => parse_threshold(val)?,
                _ => bail!("threshold object needs bits or value"),
            }
        }
        _ => bail!("bad threshold {v}"),
    };
    ensure!(!t.is_nan() && t != f64::NEG_INFINITY, "bad threshold {t}");
    Ok(t)
}

/// Operating points `{"0.01": t, "0.001": t}` (extra keys are ignored).
pub fn parse_operating_points(v: &Value) -> Result<BTreeMap<&'static str, f64>> {
    let m = v
        .get("thresholds")
        .unwrap_or(v)
        .as_object()
        .context("operating points must be an object")?;
    let mut out = BTreeMap::new();
    for a in ALPHAS {
        if let Some(t) = m.get(a.label) {
            out.insert(a.label, parse_threshold(t)?);
        }
    }
    ensure!(
        !out.is_empty(),
        "operating points name neither 0.01 nor 0.001"
    );
    Ok(out)
}

/// What a missing or failed observation scores as.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Missing {
    /// Execution failure rule: involvement `1 - y`, human flagged, positive missed.
    Worst,
    /// Missing comparator rule on its subsample: involvement equals the label.
    Best,
}

pub struct SeedInput {
    pub seed: u64,
    pub predictions: BTreeMap<String, Obs>,
    /// CAL-declared thresholds; `None` uses the in-split diagnostic threshold.
    pub operating_points: Option<BTreeMap<&'static str, f64>>,
}

pub struct SystemInput {
    pub name: String,
    pub seeds: Vec<SeedInput>,
    /// Pre-registered subsample selection (all selected ids, observed or not).
    pub subsample: Option<BTreeSet<String>>,
    pub missing: Missing,
}

pub struct Options {
    pub released_seed: u64,
    pub held_out_generators: Vec<String>,
    /// The frontier roster (pinned in the seal). Empty: the distinct labels of
    /// the evaluated split's frontier-population rows.
    pub frontier_labels: Vec<String>,
    /// Slice dimensions without bootstrap intervals (point estimates only).
    pub no_interval_dimensions: BTreeSet<String>,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            released_seed: RELEASED_SEED,
            held_out_generators: vec![],
            frontier_labels: vec![],
            no_interval_dimensions: [
                "voice_cluster",
                "human_style_axis",
                "held_out_voice_cluster",
            ]
            .into_iter()
            .map(String::from)
            .collect(),
        }
    }
}

/// Refuse to score a split that needs CAL-declared operating points (Test,
/// Tier C condition 2) unless every seed of every system declares both.
pub fn check_operating_points(split: &str, systems: &[SystemInput]) -> Result<()> {
    if !DECLARED_OPERATING_POINT_SPLITS.contains(&split) {
        return Ok(());
    }
    for sys in systems {
        for si in &sys.seeds {
            let ok = si
                .operating_points
                .as_ref()
                .is_some_and(|m| ALPHAS.iter().all(|a| m.contains_key(a.label)));
            ensure!(
                ok,
                "split {split} needs the CAL-declared operating points (0.01 and 0.001) for {} seed {}; in-split thresholds are not allowed",
                sys.name,
                si.seed
            );
        }
    }
    Ok(())
}

/// Refuse a sealed or single-use split unless the caller names it explicitly.
pub fn check_split_permission(split: &str, consume: Option<&str>) -> Result<()> {
    if SEALED_SPLITS.contains(&split) {
        ensure!(
            consume == Some(split),
            "split {split} is sealed or single-use; scoring it needs --consume-sealed-split {split}"
        );
    } else {
        ensure!(
            consume.is_none(),
            "--consume-sealed-split given for unsealed split {split}"
        );
    }
    Ok(())
}

// ---------------------------------------------------------------- scoring

/// Per-document scored output for one system and seed.
#[derive(Clone, Debug)]
pub struct Score {
    pub inv: f64,
    /// Operating-point value: the calibrated involvement as emitted, read as
    /// `1 - p_human`. Calibrators and the runtime write `p_human = 1 - p` and
    /// split `p` into `p_model_only` and `p_mixed`; re-adding the split adds
    /// rounding noise that can separate documents of one isotonic level by an
    /// ulp, so thresholds and flags never read the re-added sum. For `p >= 0.5`
    /// `1 - p_human` is exactly `p`.
    pub flag: f64,
    pub p3: [f64; 3],
    pub share: f64,
    /// Argmax of the conditional AI scores over the full vocabulary.
    pub attr: Option<String>,
    /// Top-1 among the frontier labels and `other_ai` (every non-frontier
    /// label's mass collapsed into `other_ai`).
    pub attr_frontier: Option<String>,
    /// Completed role results: human and AI profile returned.
    pub human_profile: Option<String>,
    pub ai_profile: Option<String>,
    /// Role involvement probabilities: human contributor present, AI present.
    pub role_inv: [f64; 2],
    pub failed: bool,
    /// Projected-share consistency: share within [p_model_only, involvement].
    pub share_consistent: bool,
}

impl Score {
    fn loss(&self, r: &Row) -> f64 {
        (self.inv - r.y).powi(2)
    }
}

fn argmax_label(m: &BTreeMap<String, f64>) -> Option<String> {
    // BTreeMap order + strict comparison: ties go to the smallest label.
    let mut best: Option<(&String, f64)> = None;
    for (k, &v) in m {
        if best.is_none_or(|(_, b)| v > b) {
            best = Some((k, v));
        }
    }
    best.map(|(k, _)| k.clone())
}

/// Top-1 over the frontier labels in the vocabulary plus `other_ai`, whose
/// score is all remaining conditional mass (ties to the smallest label).
pub fn frontier_argmax(m: &BTreeMap<String, f64>, frontier: &BTreeSet<String>) -> Option<String> {
    if m.is_empty() {
        return None;
    }
    let mut c: BTreeMap<String, f64> = m
        .iter()
        .filter(|(k, _)| frontier.contains(*k))
        .map(|(k, v)| (k.clone(), *v))
        .collect();
    let rest: f64 = m
        .iter()
        .filter(|(k, _)| !frontier.contains(*k))
        .map(|(_, v)| v)
        .sum();
    c.insert(OTHER_AI.to_string(), rest);
    argmax_label(&c)
}

/// Whether the row documents a contributor of the role (human: human-only
/// or mixed; ai: model-only or mixed).
fn documents_role(r: &Row, role: usize) -> bool {
    match role {
        0 => r.origin != 1,
        _ => r.origin != 0,
    }
}

fn score_obs(r: &Row, obs: Option<&Obs>, missing: Missing, frontier: &BTreeSet<String>) -> Score {
    let documented = [
        documents_role(r, 0) as u8 as f64,
        documents_role(r, 1) as u8 as f64,
    ];
    match obs {
        Some(Obs::Ok(p)) => {
            let inv = (p.p_model_only + p.p_mixed).min(1.0);
            let role = |k: &str| p.roles.get(k);
            Score {
                inv,
                flag: (1.0 - p.p_human).clamp(0.0, 1.0),
                p3: [p.p_human, p.p_model_only, p.p_mixed],
                share: p.share,
                attr: argmax_label(&p.attribution),
                attr_frontier: frontier_argmax(&p.attribution, frontier),
                human_profile: role("human").map(|o| o.profile.clone()),
                ai_profile: role("ai").map(|o| o.profile.clone()),
                role_inv: [
                    role("human")
                        .and_then(|o| o.involvement_probability)
                        .unwrap_or((p.p_human + p.p_mixed).min(1.0)),
                    role("ai")
                        .and_then(|o| o.involvement_probability)
                        .unwrap_or(inv),
                ],
                failed: false,
                share_consistent: p.share >= p.p_model_only - 1e-9 && p.share <= inv + 1e-9,
            }
        }
        _ => match missing {
            Missing::Worst => {
                let mut p3 = [0.0; 3];
                p3[if r.is_human() { 1 } else { 0 }] = 1.0;
                Score {
                    inv: 1.0 - r.y,
                    flag: 1.0 - r.y,
                    p3,
                    share: if r.share >= 0.5 { 0.0 } else { 1.0 },
                    attr: None,
                    attr_frontier: None,
                    human_profile: None,
                    ai_profile: None,
                    role_inv: [1.0 - documented[0], 1.0 - documented[1]],
                    failed: true,
                    share_consistent: true,
                }
            }
            Missing::Best => {
                let mut p3 = [0.0; 3];
                p3[r.origin] = 1.0;
                Score {
                    inv: r.y,
                    flag: r.y,
                    p3,
                    share: r.share,
                    attr: r.label.clone(),
                    attr_frontier: None,
                    human_profile: None,
                    ai_profile: None,
                    role_inv: documented,
                    failed: true,
                    share_consistent: true,
                }
            }
        },
    }
}

/// Scores aligned to population rows; `None` marks a document outside the
/// system's subsample.
struct Scored {
    seed: u64,
    scores: Vec<Option<Score>>,
    ops: BTreeMap<&'static str, (f64, &'static str)>,
    failures: usize,
    vocab: Option<BTreeSet<String>>,
    /// Per role (human, ai): the roster of named profiles, when the system
    /// returns that role's completed result.
    rosters: [Option<BTreeSet<String>>; 2],
}

// ---------------------------------------------------------------- weights

/// Balanced weights over `idx`: equal group mass, equal observed-cell mass
/// within a group, equal document mass within a cell (sum 1).
/// Same rule as `pangram_subsample::balanced_weights` (tested equal), without
/// copying group keys.
pub fn balanced_weights(rows: &[Row], idx: &[usize]) -> Vec<f64> {
    let mut cells: BTreeMap<(&str, Cell), usize> = BTreeMap::new();
    for &i in idx {
        *cells
            .entry((rows[i].group.as_str(), rows[i].cell))
            .or_default() += 1;
    }
    let mut per_group: BTreeMap<&str, usize> = BTreeMap::new();
    for (g, _) in cells.keys() {
        *per_group.entry(g).or_default() += 1;
    }
    let groups = per_group.len() as f64;
    idx.iter()
        .map(|&i| {
            let g = rows[i].group.as_str();
            1.0 / (groups * per_group[g] as f64 * cells[&(g, rows[i].cell)] as f64)
        })
        .collect()
}

/// Family-weighted: equal group mass, equal document mass within a group.
pub fn family_weights(rows: &[Row], idx: &[usize]) -> Vec<f64> {
    let mut n: BTreeMap<&str, usize> = BTreeMap::new();
    for &i in idx {
        *n.entry(rows[i].group.as_str()).or_default() += 1;
    }
    let g = n.len() as f64;
    idx.iter()
        .map(|&i| 1.0 / (g * n[rows[i].group.as_str()] as f64))
        .collect()
}

/// Inclusion probabilities aligned to the split's rows (`None` outside the
/// subsample); `None` overall means every document is observed.
type Pi<'a> = Option<&'a [Option<f64>]>;

/// Estimation weights for the observed documents `obs` of a document set
/// `pop`: the balanced (or family) weights over `pop`, each divided by the
/// document's inclusion probability. With full observation (`pi` None, `obs`
/// equal to `pop`) these are the plain weights over `pop`; with a subsample
/// they make every weighted sum the Horvitz-Thompson estimate of the
/// full-population value (unobserved documents contribute 0).
pub fn est_weights(rows: &[Row], pop: &[usize], obs: &[usize], pi: Pi, family: bool) -> Vec<f64> {
    let w = if family {
        family_weights(rows, pop)
    } else {
        balanced_weights(rows, pop)
    };
    let Some(pi) = pi else {
        assert_eq!(pop, obs, "full observation needs obs == pop");
        return w;
    };
    let at: HashMap<usize, f64> = pop.iter().copied().zip(w).collect();
    obs.iter()
        .map(|i| at[i] / pi[*i].expect("observed document without inclusion probability"))
        .collect()
}

/// Group values `v_g = G * sum_{d in g, observed} w_d x_d` over the groups of
/// `pop` in group-key order (a group without an observed document is 0);
/// their mean is the weighted sum and they are the bootstrap units.
pub fn group_values(rows: &[Row], pop: &[usize], obs: &[usize], w: &[f64], x: &[f64]) -> Vec<f64> {
    let mut m: BTreeMap<&str, f64> = BTreeMap::new();
    for &i in pop {
        m.entry(rows[i].group.as_str()).or_default();
    }
    let g = m.len() as f64;
    for (k, &i) in obs.iter().enumerate() {
        *m.get_mut(rows[i].group.as_str())
            .expect("observed document outside its population") += g * w[k] * x[k];
    }
    m.into_values().collect()
}

fn wmean(w: &[f64], x: &[f64]) -> f64 {
    w.iter().zip(x).map(|(a, b)| a * b).sum()
}

/// The protocol bootstrap generator (xorshift64 <<13 >>7 <<17, seed
/// 20261026, 1,000 draws, one index per unit per draw) applied to an
/// arbitrary statistic of the resampled unit indices. Returns sorted draws.
pub fn bootstrap_draws(n: usize, mut stat: impl FnMut(&[usize]) -> f64) -> Option<Vec<f64>> {
    if n < 2 {
        return None;
    }
    let mut seed = 20261026u64;
    let mut buf = vec![0usize; n];
    let mut draws = Vec::with_capacity(1000);
    for _ in 0..1000 {
        for b in buf.iter_mut() {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            *b = seed as usize % n;
        }
        draws.push(stat(&buf));
    }
    draws.sort_by(f64::total_cmp);
    Some(draws)
}

fn ci_json(ci: Option<(f64, f64)>) -> Value {
    match ci {
        Some((lo, hi)) => {
            json!({"lower": lo, "upper": hi, "method": "protocol_source_group_bootstrap_1000_seed20261026_draws24_974"})
        }
        None => Value::Null,
    }
}

type CpKey = (u64, u64, u64, bool);
fn cp_cache() -> &'static Mutex<HashMap<CpKey, f64>> {
    static C: OnceLock<Mutex<HashMap<CpKey, f64>>> = OnceLock::new();
    C.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Memoized one-sided Clopper-Pearson upper bound (slices share counts).
fn cp_upper(k: u64, n: u64, level: f64) -> f64 {
    let key = (k, n, level.to_bits(), false);
    if let Some(v) = cp_cache().lock().unwrap().get(&key) {
        return *v;
    }
    let v = clopper_pearson_upper(k, n, level);
    cp_cache().lock().unwrap().insert(key, v);
    v
}

/// Memoized Tier C flag limit: largest k with one-sided 95% bound <= 2.5%.
fn flag_limit(n: u64) -> Option<u64> {
    let key = (0, n, 0, true);
    if let Some(v) = cp_cache().lock().unwrap().get(&key) {
        return (*v >= 0.0).then_some(*v as u64);
    }
    let v = max_flags(n, 0.95, 0.025);
    cp_cache()
        .lock()
        .unwrap()
        .insert(key, v.map_or(-1.0, |x| x as f64));
    v
}

/// One-sided Clopper-Pearson lower bound at `level`.
pub fn clopper_pearson_lower(k: u64, n: u64, level: f64) -> f64 {
    if k == 0 {
        return 0.0;
    }
    1.0 - cp_upper(n - k, n, level)
}

fn rate_json(k: usize, n: usize, with_limit: bool) -> Value {
    if n == 0 {
        return json!({"count": k, "n": 0, "rate": null});
    }
    let (k64, n64) = (k as u64, n as u64);
    let mut v = json!({
        "count": k, "n": n, "rate": k as f64 / n as f64,
        "clopper_pearson_upper_one_sided_95": cp_upper(k64, n64, 0.95),
        "clopper_pearson_two_sided_95": [clopper_pearson_lower(k64, n64, 0.975), cp_upper(k64, n64, 0.975)],
    });
    if with_limit {
        v["tier_c_flag_limit_cp95_at_2_5pct"] = json!(flag_limit(n64));
    }
    v
}

/// Protocol operating point: the smallest value `v` among the distinct
/// values of `all` (any origin) plus +inf such that at most `alpha` of the
/// human values are `>= v` (each human once, unweighted).
pub fn operating_threshold(all: &[f64], humans: &[f64], alpha: Alpha) -> f64 {
    let mut h: Vec<f64> = humans.to_vec();
    h.sort_by(f64::total_cmp);
    let mut v: Vec<f64> = all.to_vec();
    v.sort_by(f64::total_cmp);
    v.dedup_by(|a, b| a.to_bits() == b.to_bits() || a == b);
    let nh = h.len() as u64;
    for t in v {
        let below = h.partition_point(|x| *x < t);
        let flagged = (h.len() - below) as u64;
        if flagged * alpha.den <= alpha.num * nh {
            return t;
        }
    }
    f64::INFINITY
}

fn threshold_json(t: f64, source: &str) -> Value {
    json!({
        "value": if t.is_finite() { json!(t) } else { json!("inf") },
        "bits": format!("0x{:016x}", t.to_bits()),
        "source": source,
    })
}

// ---------------------------------------------------------------- metrics

fn ece10(
    rows: &[Row],
    pop: &[usize],
    obs: &[usize],
    s: &[&Score],
    pi: Pi,
) -> Option<(f64, Vec<Value>)> {
    if obs.is_empty() {
        return None;
    }
    let w = est_weights(rows, pop, obs, pi, false);
    let mut bins = [(0.0f64, 0.0f64, 0.0f64, 0usize); 10];
    for (k, &i) in obs.iter().enumerate() {
        let p = s[k].inv;
        let b = &mut bins[((p * 10.0).floor() as usize).min(9)];
        b.0 += w[k];
        b.1 += w[k] * p;
        b.2 += w[k] * rows[i].y;
        b.3 += 1;
    }
    let ece = bins.iter().map(|b| (b.1 - b.2).abs()).sum();
    let detail = bins
        .iter()
        .enumerate()
        .filter(|(_, b)| b.3 > 0)
        .map(|(i, b)| json!({"bin": i, "documents": b.3, "weight": b.0, "mean_involvement": b.1 / b.0, "observed_rate": b.2 / b.0}))
        .collect();
    Some((ece, detail))
}

/// The members of `idx` satisfying `f`.
fn sel(rows: &[Row], idx: &[usize], f: impl Fn(&Row) -> bool) -> Vec<usize> {
    idx.iter().copied().filter(|&i| f(&rows[i])).collect()
}

fn share_key(r: &Row) -> String {
    match r.cell {
        Cell::Mixed => format!("mixed:{}", SHARE_BINS[ps::share_bin(r.share).unwrap()]),
        c => c.name().to_string(),
    }
}

fn share_metrics(rows: &[Row], pop: &[usize], obs: &[usize], s: &[&Score], pi: Pi) -> Value {
    let score: HashMap<usize, &Score> = obs.iter().copied().zip(s.iter().copied()).collect();
    let block = |f: &dyn Fn(&Row) -> bool| -> Value {
        let pb = sel(rows, pop, f);
        let ob = sel(rows, obs, f);
        if ob.is_empty() {
            return Value::Null;
        }
        let w = est_weights(rows, &pb, &ob, pi, false);
        let e: Vec<f64> = ob
            .iter()
            .map(|i| (score[i].share - rows[*i].share) * 100.0)
            .collect();
        let abs: Vec<f64> = e.iter().map(|x| x.abs()).collect();
        let sq: Vec<f64> = e.iter().map(|x| x * x).collect();
        json!({"documents": ob.len(), "mae_points": wmean(&w, &abs), "mse_points_squared": wmean(&w, &sq), "bias_points": wmean(&w, &e)})
    };
    let keys: BTreeSet<String> = obs.iter().map(|&i| share_key(&rows[i])).collect();
    let inconsistent = s.iter().filter(|x| !x.share_consistent).count();
    json!({
        "weighting": if pi.is_some() { "horvitz_thompson_balanced_within_each_block" } else { "balanced_within_each_block" },
        "overall": block(&|_| true),
        "mixed": block(&|r| r.cell == Cell::Mixed),
        "by_realised_share": keys.iter().map(|k| (k.clone(), block(&|r| share_key(r) == *k))).collect::<Map<_, _>>(),
        "share_outside_model_only_to_involvement": inconsistent,
    })
}

fn f1(tp: usize, pred: usize, sup: usize) -> (f64, f64, f64) {
    let p = if pred == 0 {
        0.0
    } else {
        tp as f64 / pred as f64
    };
    let r = if sup == 0 {
        0.0
    } else {
        tp as f64 / sup as f64
    };
    let f = if p + r == 0.0 {
        0.0
    } else {
        2.0 * p * r / (p + r)
    };
    (p, r, f)
}

/// Unweighted macro-F1 over the union of target and predicted labels
/// (precision 0 for a label never predicted, F1 0 when undefined).
pub fn macro_f1(pairs: &[(String, Option<String>)]) -> (f64, BTreeMap<String, Value>) {
    let mut labels = BTreeSet::new();
    for (t, p) in pairs {
        labels.insert(t.clone());
        if let Some(p) = p {
            labels.insert(p.clone());
        }
    }
    let mut per = BTreeMap::new();
    let mut sum = 0.0;
    for l in &labels {
        let sup = pairs.iter().filter(|(t, _)| t == l).count();
        let pred = pairs.iter().filter(|(_, p)| p.as_ref() == Some(l)).count();
        let tp = pairs
            .iter()
            .filter(|(t, p)| t == l && p.as_ref() == Some(l))
            .count();
        let (p, r, f) = f1(tp, pred, sup);
        sum += f;
        per.insert(
            l.clone(),
            json!({"support": sup, "predicted": pred, "correct": tp, "precision": p, "recall": r, "f1": f}),
        );
    }
    (
        if labels.is_empty() {
            0.0
        } else {
            sum / labels.len() as f64
        },
        per,
    )
}

fn three_way(rows: &[Row], pop: &[usize], idx: &[usize], s: &[&Score], pi: Pi) -> Value {
    if idx.is_empty() {
        return Value::Null;
    }
    let w = est_weights(rows, pop, idx, pi, false);
    let mut brier = vec![];
    let mut ll = vec![];
    let mut acc = vec![];
    let mut confusion = [[0usize; 3]; 3];
    let mut pairs = vec![];
    for (k, &i) in idx.iter().enumerate() {
        let t = rows[i].origin;
        let p = s[k].p3;
        brier.push(
            (0..3)
                .map(|c| (p[c] - if c == t { 1.0 } else { 0.0 }).powi(2))
                .sum(),
        );
        ll.push(-p[t].max(1e-15).ln());
        let mut a = 0;
        for c in 1..3 {
            if p[c] > p[a] {
                a = c;
            }
        }
        acc.push(if a == t { 1.0 } else { 0.0 });
        confusion[t][a] += 1;
        pairs.push((ORIGINS[t].to_string(), Some(ORIGINS[a].to_string())));
    }
    let (mf1, per) = macro_f1(&pairs);
    json!({
        "class_order": ORIGINS,
        "balanced_brier_sum3": wmean(&w, &brier),
        "balanced_log_loss": wmean(&w, &ll),
        "balanced_accuracy_weighted": wmean(&w, &acc),
        "accuracy_unweighted": acc.iter().sum::<f64>() / acc.len() as f64,
        "macro_f1_unweighted": mf1,
        "per_class": per,
        "confusion_rows_true_cols_predicted": confusion,
    })
}

fn attribution(
    rows: &[Row],
    idx: &[usize],
    s: &[&Score],
    vocab: Option<&BTreeSet<String>>,
) -> Value {
    let Some(vocab) = vocab else {
        return json!({"available": false});
    };
    let mut pairs = vec![];
    let mut by_label: BTreeMap<String, (usize, usize, String)> = BTreeMap::new();
    let mut remapped = 0;
    for (k, &i) in idx.iter().enumerate() {
        let Some(label) = &rows[i].label else {
            continue;
        };
        let target = if vocab.contains(label) {
            label.clone()
        } else {
            remapped += 1;
            OTHER_AI.to_string()
        };
        let pred = s[k].attr.clone();
        let e = by_label
            .entry(label.clone())
            .or_insert((0, 0, target.clone()));
        e.0 += 1;
        e.1 += (pred.as_ref() == Some(&target)) as usize;
        pairs.push((target, pred));
    }
    if pairs.is_empty() {
        return json!({"available": true, "documents": 0});
    }
    let correct = pairs.iter().filter(|(t, p)| p.as_ref() == Some(t)).count();
    let (mf1, per) = macro_f1(&pairs);
    let recalls: Vec<f64> = per
        .values()
        .filter(|v| v["support"].as_u64().unwrap() > 0)
        .map(|v| v["recall"].as_f64().unwrap())
        .collect();
    json!({
        "available": true,
        "documents": pairs.len(),
        "targets_remapped_to_other_ai": remapped,
        "accuracy": correct as f64 / pairs.len() as f64,
        "label_balanced_accuracy": recalls.iter().sum::<f64>() / recalls.len() as f64,
        "macro_f1": mf1,
        "per_target_label": per,
        "per_generator": by_label.iter().map(|(l, (n, c, t))| (l.clone(), json!({"documents": n, "correct": c, "accuracy": *c as f64 / *n as f64, "target": t}))).collect::<Map<_, _>>(),
    })
}

/// Frontier attribution as the protocol states it: top-1 among the frontier
/// labels and `other_ai`, on the frontier positives of `idx`. The target is
/// the row's label when it is a frontier label of the model's vocabulary,
/// otherwise `other_ai`. Reported over all voices and over held-out voices.
fn frontier_attribution(
    rows: &[Row],
    idx: &[usize],
    s: &[&Score],
    vocab: Option<&BTreeSet<String>>,
    frontier: &BTreeSet<String>,
) -> Value {
    let Some(vocab) = vocab else {
        return json!({"available": false});
    };
    let block = |held: bool| -> Value {
        let mut per: BTreeMap<String, (usize, usize, String)> = BTreeMap::new();
        let mut pairs = vec![];
        for (k, &i) in idx.iter().enumerate() {
            let r = &rows[i];
            if !r.is_frontier() || (held && !r.held_out_style) {
                continue;
            }
            let label = r.label.clone().unwrap();
            let target = if vocab.contains(&label) && frontier.contains(&label) {
                label.clone()
            } else {
                OTHER_AI.to_string()
            };
            let pred = s[k].attr_frontier.clone();
            let e = per.entry(label).or_insert((0, 0, target.clone()));
            e.0 += 1;
            e.1 += (pred.as_ref() == Some(&target)) as usize;
            pairs.push((target, pred));
        }
        if pairs.is_empty() {
            return json!({"documents": 0});
        }
        let correct = pairs.iter().filter(|(t, p)| p.as_ref() == Some(t)).count();
        let (mf1, _) = macro_f1(&pairs);
        json!({
            "documents": pairs.len(),
            "correct": correct,
            "accuracy": correct as f64 / pairs.len() as f64,
            "macro_f1": mf1,
            "per_frontier_label": per.iter().map(|(l, (n, c, t))| (l.clone(), json!({"documents": n, "correct": c, "accuracy": *c as f64 / *n as f64, "target": t}))).collect::<Map<_, _>>(),
        })
    };
    json!({
        "available": true,
        "rule": "top1_among_frontier_labels_and_other_ai",
        "held_out_voices": block(true),
        "all_voices": block(false),
    })
}

/// Completed human- and AI-role attribution, scored on every document that
/// documents a contributor of the role, regardless of predicted involvement.
/// The target is the documented contributor's profile when the role's roster
/// holds it, otherwise `other_human` or `other_ai`; the prediction is the
/// returned profile. Role involvement (contributor present) is scored with a
/// balanced Brier over every document.
fn role_attribution(
    rows: &[Row],
    pop: &[usize],
    obs: &[usize],
    s: &[&Score],
    pi: Pi,
    rosters: &[Option<BTreeSet<String>>; 2],
    vocab: Option<&BTreeSet<String>>,
) -> Value {
    let mut out = Map::new();
    let w = est_weights(rows, pop, obs, pi, false);
    for (k, role) in ROLES.iter().enumerate() {
        let loss: Vec<f64> = obs
            .iter()
            .zip(s)
            .map(|(&i, sc)| (sc.role_inv[k] - documents_role(&rows[i], k) as u8 as f64).powi(2))
            .collect();
        let brier = (!obs.is_empty()).then(|| wmean(&w, &loss));
        // The AI role falls back to the argmax of the conditional scores when
        // the system returns no completed AI result.
        let (roster, source) = match (&rosters[k], k, vocab) {
            (Some(r), _, _) => (Some(r.clone()), "returned_profile"),
            (None, 1, Some(v)) => (Some(v.clone()), "argmax_of_conditional_scores"),
            _ => (None, "unavailable"),
        };
        let Some(roster) = roster else {
            out.insert(
                role.to_string(),
                json!({"available": false, "role_involvement_balanced_brier": brier}),
            );
            continue;
        };
        let fallback = role_fallback(role);
        let mut pairs = vec![];
        let mut remapped = 0;
        let mut undocumented = 0;
        for (j, &i) in obs.iter().enumerate() {
            let r = &rows[i];
            if !documents_role(r, k) {
                undocumented += 1;
                continue;
            }
            let documented = if k == 0 {
                r.human_label.clone()
            } else {
                r.label.clone()
            };
            let target = match documented {
                Some(l) if roster.contains(&l) => l,
                Some(_) => {
                    remapped += 1;
                    fallback.to_string()
                }
                None => fallback.to_string(),
            };
            let pred = match (k, source) {
                (0, _) => s[j].human_profile.clone(),
                (_, "returned_profile") => s[j].ai_profile.clone(),
                _ => s[j].attr.clone(),
            };
            pairs.push((target, pred));
        }
        let correct = pairs.iter().filter(|(t, p)| p.as_ref() == Some(t)).count();
        let (mf1, per) = macro_f1(&pairs);
        out.insert(
            role.to_string(),
            json!({
                "available": true,
                "prediction": source,
                "roster": roster,
                "fallback": fallback,
                "documented_documents": pairs.len(),
                "undocumented_documents_not_scored": undocumented,
                "targets_remapped_to_fallback": remapped,
                "accuracy": (!pairs.is_empty()).then(|| correct as f64 / pairs.len() as f64),
                "macro_f1": (!pairs.is_empty()).then_some(mf1),
                "per_target_label": per,
                "role_involvement_balanced_brier": brier,
            }),
        );
    }
    Value::Object(out)
}

/// Core detection metrics of one seed over the document set `pop`, observed
/// on `obs` (equal to `pop` unless the system is a subsample, `pi` Some).
/// Weighted metrics are Horvitz-Thompson estimates of the `pop` values for a
/// subsample; counts and rates (FPR, TPR) are over the observed documents.
#[allow(clippy::too_many_arguments)]
fn detection(
    rows: &[Row],
    pop: &[usize],
    idx: &[usize],
    s: &[&Score],
    pi: Pi,
    ops: &BTreeMap<&'static str, (f64, &'static str)>,
    with_ci: bool,
    compact: bool,
) -> Value {
    let loss: Vec<f64> = idx
        .iter()
        .zip(s)
        .map(|(&i, sc)| sc.loss(&rows[i]))
        .collect();
    let wb = est_weights(rows, pop, idx, pi, false);
    let wf = est_weights(rows, pop, idx, pi, true);
    let gv = group_values(rows, pop, idx, &wb, &loss);
    let mut groups = BTreeSet::new();
    let mut counts = [0usize; 3];
    let mut fails = 0;
    for (k, &i) in idx.iter().enumerate() {
        groups.insert(rows[i].group.as_str());
        counts[rows[i].origin] += 1;
        fails += s[k].failed as usize;
    }
    let humans: Vec<usize> = (0..idx.len())
        .filter(|&k| rows[idx[k]].is_human())
        .collect();
    let pos: Vec<usize> = (0..idx.len())
        .filter(|&k| !rows[idx[k]].is_human())
        .collect();
    let human_groups: BTreeSet<&str> = humans
        .iter()
        .map(|&k| rows[idx[k]].group.as_str())
        .collect();
    let flagged = |k: usize, t: f64| s[k].flag >= t;
    let fpr = |t: f64| -> Value {
        let n = humans.len();
        let c = humans.iter().filter(|&&k| flagged(k, t)).count();
        if compact {
            return json!({"count": c, "n": n, "rate": (n > 0).then(|| c as f64 / n as f64), "clopper_pearson_upper_one_sided_95": (n > 0).then(|| cp_upper(c as u64, n as u64, 0.95))});
        }
        let mut v = rate_json(c, n, true);
        if with_ci && human_groups.len() >= 2 {
            // Ratio bootstrap over source groups that contain humans.
            let mut per: BTreeMap<&str, (f64, f64)> = BTreeMap::new();
            for &k in &humans {
                let e = per.entry(rows[idx[k]].group.as_str()).or_default();
                e.0 += flagged(k, t) as u8 as f64;
                e.1 += 1.0;
            }
            let units: Vec<(f64, f64)> = per.into_values().collect();
            let d = bootstrap_draws(units.len(), |ix| {
                let (a, b) = ix.iter().fold((0.0, 0.0), |acc, &j| {
                    (acc.0 + units[j].0, acc.1 + units[j].1)
                });
                a / b
            })
            .unwrap();
            v["source_group_bootstrap_95"] = json!([d[24], d[974]]);
        }
        v
    };
    let tpr = |t: f64| -> Value {
        let c = pos.iter().filter(|&&k| flagged(k, t)).count();
        if compact {
            return json!({"count": c, "n": pos.len(), "rate": (!pos.is_empty()).then(|| c as f64 / pos.len() as f64)});
        }
        rate_json(c, pos.len(), false)
    };
    let mut op = Map::new();
    for a in ALPHAS {
        if let Some(&(t, src)) = ops.get(a.label) {
            let th = if compact {
                Value::Null
            } else {
                threshold_json(t, src)
            };
            op.insert(
                a.label.into(),
                json!({"threshold": th, "human_fpr": fpr(t), "tpr": tpr(t)}),
            );
        }
    }
    let mut ece_all = ece10(rows, pop, idx, s, pi);
    if compact && let Some(e) = &mut ece_all {
        e.1.clear();
    }
    let hpop = sel(rows, pop, Row::is_human);
    let hidx: Vec<usize> = humans.iter().map(|&k| idx[k]).collect();
    let hs: Vec<&Score> = humans.iter().map(|&k| s[k]).collect();
    let ece_h = ece10(rows, &hpop, &hidx, &hs, pi);
    let pos_loss: Vec<f64> = pos.iter().map(|&k| loss[k]).collect();
    let hloss: Vec<f64> = humans.iter().map(|&k| loss[k]).collect();
    json!({
        "documents": idx.len(),
        "population_documents": pop.len(),
        "estimator": if pi.is_some() { "horvitz_thompson_over_subsample" } else { "full_population" },
        "source_groups": groups.len(),
        "origin_counts": {"human_only": counts[0], "model_only": counts[1], "mixed": counts[2]},
        "execution_failures": fails,
        "balanced_involvement_brier": if idx.is_empty() { Value::Null } else { json!(wmean(&wb, &loss)) },
        "balanced_involvement_brier_95ci": if with_ci { ci_json(bootstrap_interval(&gv)) } else { Value::Null },
        "family_weighted_involvement_brier": if idx.is_empty() { Value::Null } else { json!(wmean(&wf, &loss)) },
        "human_mean_brier_balanced": if humans.is_empty() { Value::Null } else { json!(wmean(&est_weights(rows, &hpop, &hidx, pi, false), &hloss)) },
        "human_mean_involvement_unweighted": mean(&hs.iter().map(|x| x.inv).collect::<Vec<_>>()),
        "positive_mean_brier_unweighted": mean(&pos_loss),
        "human_fpr_at_0_5": fpr(0.5),
        "tpr_at_0_5": tpr(0.5),
        "operating_points": op,
        "ece10": ece_all.as_ref().map(|e| e.0),
        "ece10_humans": ece_h.as_ref().map(|e| e.0),
        "calibration_bins": if compact { Value::Null } else { json!(ece_all.map(|e| e.1)) },
    })
}

// ---------------------------------------------------------------- slices

/// A named document subset of the evaluated split.
#[derive(Clone, Debug)]
pub struct Slice {
    pub dimension: String,
    pub value: String,
    /// `rows` (matching primary rows), `positives_with_all_humans` (matching
    /// positives plus every primary human), `humans` (matching humans) or
    /// `population_rows` (every row of a non-primary population with humans).
    pub kind: &'static str,
    pub idx: Vec<usize>,
    /// Whether the slice holds rows outside the primary population; such a
    /// slice is not estimated for a subsample system.
    pub non_primary: bool,
}

impl Slice {
    pub fn key(&self) -> String {
        format!("{}:{}", self.dimension, self.value)
    }
}

/// Every slice the protocol and the campaign decisions name. Except for the
/// `population` and `demographic:*` slices, slices hold primary rows only.
pub fn build_slices(rows: &[Row], opts: &Options) -> Vec<Slice> {
    let primary_humans: Vec<usize> = (0..rows.len())
        .filter(|&i| rows[i].primary && rows[i].is_human())
        .collect();
    let mut row_d: BTreeMap<(String, String), Vec<usize>> = BTreeMap::new();
    let mut pos_d: BTreeMap<(String, String), Vec<usize>> = BTreeMap::new();
    let mut hum_d: BTreeMap<(String, String), Vec<usize>> = BTreeMap::new();
    let mut pops: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    let k = |a: &str, b: &str| (a.to_string(), b.to_string());
    for (i, r) in rows.iter().enumerate() {
        if r.is_human() {
            for (dk, dv) in &r.demographics {
                hum_d
                    .entry(k(&format!("demographic:{dk}"), dv))
                    .or_default()
                    .push(i);
            }
        }
        if !r.primary {
            pops.entry(r.population.clone()).or_default().push(i);
            continue;
        }
        row_d.entry(k("domain", &r.domain)).or_default().push(i);
        if let Some(g) = &r.genre {
            row_d.entry(k("genre", g)).or_default().push(i);
        }
        if let Some(t) = &r.ocr_tier {
            row_d.entry(k("ocr_tier", t)).or_default().push(i);
        }
        if let Some(m) = r.markup_removed {
            row_d
                .entry(k("removed_markup", if m { "present" } else { "absent" }))
                .or_default()
                .push(i);
        }
        if r.is_human() {
            hum_d
                .entry(k(
                    "human_genre",
                    r.genre.as_deref().unwrap_or("unspecified"),
                ))
                .or_default()
                .push(i);
            for a in &r.axes {
                hum_d.entry(k("human_style_axis", a)).or_default().push(i);
            }
            continue;
        }
        let mut add = |d: &str, v: &str| pos_d.entry(k(d, v)).or_default().push(i);
        let label = r.label.as_deref().unwrap();
        add("generator", label);
        if let Some(f) = &r.family {
            add("generator_family", f);
        }
        if r.is_frontier() {
            add("frontier", "all");
            add("frontier_model", label);
            add(
                "frontier_voicing",
                if r.voice_kind.is_some() {
                    "voiced"
                } else {
                    "plain"
                },
            );
        }
        add("cohort", r.cohort.as_deref().unwrap_or("unspecified"));
        add("operation", r.operation.as_deref().unwrap());
        if let Some(c) = &r.voice_cluster {
            add("voice_cluster", c);
        }
        if let Some(o) = &r.voice_origin {
            add("voice_origin", o);
        }
        for a in &r.axes {
            add("style_axis", a);
        }
        add(
            "held_out_style",
            if r.held_out_style { "true" } else { "false" },
        );
        if r.held_out_style {
            add("generator_held_out_style", label);
            for a in &r.axes {
                add("held_out_style_axis", a);
            }
            if let Some(c) = &r.voice_cluster {
                add("held_out_voice_cluster", c);
            }
        }
        if opts.held_out_generators.iter().any(|g| g == label) {
            add("held_out_generator", "all");
        }
        add("realised_share", &share_key(r));
    }
    let mut out = vec![];
    for ((d, v), idx) in row_d {
        out.push(Slice {
            dimension: d,
            value: v,
            kind: "rows",
            idx,
            non_primary: false,
        });
    }
    for ((d, v), mut idx) in pos_d {
        idx.extend(&primary_humans);
        idx.sort();
        out.push(Slice {
            dimension: d,
            value: v,
            kind: "positives_with_all_humans",
            idx,
            non_primary: false,
        });
    }
    for ((d, v), idx) in hum_d {
        let non_primary = idx.iter().any(|&i| !rows[i].primary);
        out.push(Slice {
            dimension: d,
            value: v,
            kind: "humans",
            idx,
            non_primary,
        });
    }
    // Each non-primary population: its own rows when it has humans (human-only
    // diagnostics), otherwise its positives plus every primary human
    // (robustness).
    for (p, mut idx) in pops {
        let has_humans = idx.iter().any(|&i| rows[i].is_human());
        if !has_humans {
            idx.extend(&primary_humans);
            idx.sort();
        }
        out.push(Slice {
            dimension: "population".into(),
            value: p,
            kind: if has_humans {
                "population_rows"
            } else {
                "positives_with_all_humans"
            },
            idx,
            non_primary: true,
        });
    }
    out
}

/// Realised slice counts from pipeline metadata (independent of any system),
/// and the protocol's minimum: 15 groups and, when the slice has humans, 150
/// humans; the pooled `frontier` slice also needs 150 frontier positives.
pub fn slice_status(rows: &[Row], slc: &Slice) -> Value {
    let groups: BTreeSet<&str> = slc.idx.iter().map(|&i| rows[i].group.as_str()).collect();
    let humans = slc.idx.iter().filter(|&&i| rows[i].is_human()).count();
    let frontier = slc.idx.iter().filter(|&&i| rows[i].is_frontier()).count();
    let mut shortfall = vec![];
    if groups.len() < SLICE_MIN_GROUPS {
        shortfall.push(format!(
            "source_groups {} < {SLICE_MIN_GROUPS}",
            groups.len()
        ));
    }
    if humans > 0 && humans < SLICE_MIN_HUMANS {
        shortfall.push(format!("human_documents {humans} < {SLICE_MIN_HUMANS}"));
    }
    if slc.dimension == "frontier" && frontier < SLICE_MIN_FRONTIER_POSITIVES {
        shortfall.push(format!(
            "frontier_positives {frontier} < {SLICE_MIN_FRONTIER_POSITIVES}"
        ));
    }
    json!({
        "documents": slc.idx.len(),
        "source_groups": groups.len(),
        "human_documents": humans,
        "frontier_positives": frontier,
        "below_slice_minimum": !shortfall.is_empty(),
        "shortfall": shortfall,
    })
}

/// The protocol's fixed list of gated slices with their realised status: a
/// listed slice below its minimum, or absent, is reported ungated.
pub fn gated_slices(rows: &[Row], slices: &[Slice]) -> Vec<Value> {
    let mut want: Vec<(String, String)> = GATED_DOMAINS
        .iter()
        .map(|d| ("domain".to_string(), d.to_string()))
        .collect();
    want.push(("held_out_generator".into(), "all".into()));
    want.push(("frontier".into(), "all".into()));
    want.into_iter()
        .map(|(d, v)| {
            let key = format!("{d}:{v}");
            match slices.iter().find(|s| s.dimension == d && s.value == v) {
                Some(slc) => {
                    let st = slice_status(rows, slc);
                    let gated = !st["below_slice_minimum"].as_bool().unwrap();
                    json!({"slice": key, "dimension": d, "value": v, "status": if gated { "gated" } else { "reported_ungated" }, "realised": st})
                }
                None => json!({"slice": key, "dimension": d, "value": v, "status": "reported_ungated", "realised": null, "shortfall": ["slice has no documents"]}),
            }
        })
        .collect()
}

// ---------------------------------------------------------------- comparisons

struct Paired<'a> {
    rows: &'a [Row],
    /// Inclusion probability of each population row for the subsample side
    /// (`None` = not sampled); `None` overall = full-population comparison.
    pi: Option<Vec<Option<f64>>>,
}

impl Paired<'_> {
    /// Per-group values of the A-minus-B difference over a slice.
    fn values(
        &self,
        idx: &[usize],
        delta: &dyn Fn(usize) -> f64,
        weights: &[f64],
    ) -> Result<Vec<f64>> {
        let mut m: BTreeMap<&str, f64> = BTreeMap::new();
        for &i in idx {
            m.entry(self.rows[i].group.as_str()).or_default();
        }
        let g = m.len() as f64;
        for (k, &i) in idx.iter().enumerate() {
            let f = match &self.pi {
                None => 1.0,
                Some(pi) => match pi[i] {
                    None => continue,
                    Some(p) => 1.0 / p,
                },
            };
            *m.get_mut(self.rows[i].group.as_str()).unwrap() += g * weights[k] * delta(i) * f;
        }
        Ok(m.into_values().collect())
    }
}

/// Inclusion probabilities `n_h / N_h` over the split's primary strata.
fn inclusion(rows: &[Row], sample: &BTreeSet<String>) -> Result<Vec<Option<f64>>> {
    let mut pop: BTreeMap<String, (usize, usize)> = BTreeMap::new();
    let mut keys = vec![None; rows.len()];
    let mut seen = 0;
    for (i, r) in rows.iter().enumerate() {
        if !r.primary {
            ensure!(
                !sample.contains(&r.id),
                "subsample names non-primary document {}",
                r.id
            );
            continue;
        }
        let k = r.stratum()?;
        let e = pop.entry(k.clone()).or_default();
        e.0 += 1;
        if sample.contains(&r.id) {
            e.1 += 1;
            seen += 1;
        }
        keys[i] = Some(k);
    }
    ensure!(
        seen == sample.len(),
        "subsample names documents outside the evaluated split"
    );
    for (k, (n, s)) in &pop {
        ensure!(
            *s > 0,
            "stratum {k} ({n} documents) has no subsample document"
        );
    }
    Ok(rows
        .iter()
        .zip(keys)
        .map(|(r, k)| {
            k.filter(|_| sample.contains(&r.id)).map(|k| {
                let (n, s) = pop[&k];
                s as f64 / n as f64
            })
        })
        .collect())
}

// ---------------------------------------------------------------- driver

fn mean(v: &[f64]) -> Option<f64> {
    (!v.is_empty()).then(|| v.iter().sum::<f64>() / v.len() as f64)
}

fn get_f(v: &Value, ptr: &str) -> Option<f64> {
    v.pointer(ptr).and_then(Value::as_f64)
}

/// Evaluate every system on the population (rows of one split).
pub fn evaluate(
    rows: &[Row],
    systems: &[SystemInput],
    comparisons: &[(String, String)],
    opts: &Options,
) -> Result<Value> {
    ensure!(!rows.is_empty(), "empty evaluation population");
    let split = rows[0].split.clone();
    ensure!(
        rows.iter().all(|r| r.split == split),
        "population mixes splits"
    );
    check_operating_points(&split, systems)?;
    let pos: BTreeMap<&str, usize> = rows
        .iter()
        .enumerate()
        .map(|(i, r)| (r.id.as_str(), i))
        .collect();
    let primary: Vec<usize> = (0..rows.len()).filter(|&i| rows[i].primary).collect();
    let all_rows: Vec<usize> = (0..rows.len()).collect();
    let slices = build_slices(rows, opts);
    let status: Vec<Value> = slices.iter().map(|s| slice_status(rows, s)).collect();
    let gated = gated_slices(rows, &slices);
    // Frontier roster: declared, or the split's frontier labels.
    let split_frontier: BTreeSet<String> = rows
        .iter()
        .filter(|r| r.is_frontier())
        .filter_map(|r| r.label.clone())
        .collect();
    let (frontier, frontier_source): (BTreeSet<String>, &str) = if opts.frontier_labels.is_empty() {
        (
            split_frontier.clone(),
            "frontier_population_labels_of_split",
        )
    } else {
        let f: BTreeSet<String> = opts.frontier_labels.iter().cloned().collect();
        for l in &split_frontier {
            ensure!(
                f.contains(l),
                "frontier row label {l} is not in the declared frontier roster"
            );
        }
        (f, "declared")
    };
    let mut scored: BTreeMap<String, Vec<Scored>> = BTreeMap::new();
    let mut incl: BTreeMap<String, Vec<Option<f64>>> = BTreeMap::new();
    let mut names = BTreeSet::new();
    for sys in systems {
        ensure!(
            names.insert(sys.name.clone()),
            "duplicate system {}",
            sys.name
        );
        ensure!(
            !sys.seeds.is_empty(),
            "system {} has no predictions",
            sys.name
        );
        let mut seeds = BTreeSet::new();
        let mut out = vec![];
        if let Some(s) = &sys.subsample {
            for id in s {
                ensure!(
                    pos.contains_key(id.as_str()),
                    "{}: subsample id {id} not in split",
                    sys.name
                );
            }
            incl.insert(
                sys.name.clone(),
                inclusion(rows, s).with_context(|| format!("{} subsample", sys.name))?,
            );
        }
        for si in &sys.seeds {
            ensure!(
                seeds.insert(si.seed),
                "{}: duplicate seed {}",
                sys.name,
                si.seed
            );
            for id in si.predictions.keys() {
                ensure!(
                    pos.contains_key(id.as_str()),
                    "{} seed {}: prediction {id} is not a document of split {split}",
                    sys.name,
                    si.seed
                );
                if let Some(s) = &sys.subsample {
                    ensure!(
                        s.contains(id),
                        "{} seed {}: prediction {id} outside its subsample",
                        sys.name,
                        si.seed
                    );
                }
            }
            let mut vocab: Option<BTreeSet<String>> = None;
            let mut rosters: [Option<BTreeSet<String>>; 2] = [None, None];
            for o in si.predictions.values() {
                let Obs::Ok(p) = o else { continue };
                if !p.attribution.is_empty() {
                    let k: BTreeSet<String> = p.attribution.keys().cloned().collect();
                    if let Some(v) = &vocab {
                        ensure!(
                            *v == k,
                            "{}: attribution vocabulary differs within seed {}",
                            sys.name,
                            si.seed
                        );
                    }
                    vocab = Some(k);
                }
                for (j, role) in ROLES.iter().enumerate() {
                    if let Some(ro) = p.roles.get(*role) {
                        let k: BTreeSet<String> = ro.named.keys().cloned().collect();
                        if let Some(v) = &rosters[j] {
                            ensure!(
                                *v == k,
                                "{}: {role} roster differs within seed {}",
                                sys.name,
                                si.seed
                            );
                        }
                        rosters[j] = Some(k);
                    }
                }
            }
            let mut failures = 0;
            let scores: Vec<Option<Score>> = rows
                .iter()
                .map(|r| {
                    let observed = sys.subsample.as_ref().is_none_or(|s| s.contains(&r.id));
                    observed.then(|| {
                        let o = si.predictions.get(&r.id);
                        let sc = score_obs(r, o, sys.missing, &frontier);
                        failures += sc.failed as usize;
                        sc
                    })
                })
                .collect();
            let mut ops = BTreeMap::new();
            match &si.operating_points {
                Some(m) => {
                    for (k, t) in m {
                        ops.insert(*k, (*t, "declared"));
                    }
                }
                None => {
                    let obs: Vec<usize> = primary
                        .iter()
                        .copied()
                        .filter(|&i| scores[i].is_some())
                        .collect();
                    let all: Vec<f64> = obs
                        .iter()
                        .map(|&i| scores[i].as_ref().unwrap().flag)
                        .collect();
                    let hum: Vec<f64> = obs
                        .iter()
                        .filter(|&&i| rows[i].is_human())
                        .map(|&i| scores[i].as_ref().unwrap().flag)
                        .collect();
                    let source = if split == "cal" {
                        "in_split_cal_rule"
                    } else {
                        "in_split_diagnostic"
                    };
                    if !hum.is_empty() {
                        for a in ALPHAS {
                            ops.insert(a.label, (operating_threshold(&all, &hum, a), source));
                        }
                    }
                }
            }
            out.push(Scored {
                seed: si.seed,
                scores,
                ops,
                failures,
                vocab,
                rosters,
            });
        }
        scored.insert(sys.name.clone(), out);
    }

    let observed = |sc: &Scored, idx: &[usize]| -> Vec<usize> {
        idx.iter()
            .copied()
            .filter(|&i| sc.scores[i].is_some())
            .collect()
    };
    let scores_of = |sc: &'_ Scored, idx: &[usize]| -> Vec<Score> {
        idx.iter().map(|&i| sc.scores[i].clone().unwrap()).collect()
    };
    let mut systems_json = Map::new();
    for sys in systems {
        let ss = &scored[&sys.name];
        let pi: Pi = incl.get(&sys.name).map(|v| v.as_slice());
        let mut seeds_json = Map::new();
        let mut per_seed = vec![];
        for sc in ss {
            let idx = observed(sc, &primary);
            let owned = scores_of(sc, &idx);
            let s: Vec<&Score> = owned.iter().collect();
            let mut m = detection(rows, &primary, &idx, &s, pi, &sc.ops, true, false);
            m["share"] = share_metrics(rows, &primary, &idx, &s, pi);
            m["three_way"] = three_way(rows, &primary, &idx, &s, pi);
            m["attribution"] = attribution(rows, &idx, &s, sc.vocab.as_ref());
            let held: Vec<usize> = (0..idx.len())
                .filter(|&k| rows[idx[k]].held_out_style)
                .collect();
            let hidx: Vec<usize> = held.iter().map(|&k| idx[k]).collect();
            let hs: Vec<&Score> = held.iter().map(|&k| s[k]).collect();
            m["attribution_held_out_voices"] = attribution(rows, &hidx, &hs, sc.vocab.as_ref());
            m["frontier_attribution"] =
                frontier_attribution(rows, &idx, &s, sc.vocab.as_ref(), &frontier);
            m["role_attribution"] =
                role_attribution(rows, &primary, &idx, &s, pi, &sc.rosters, sc.vocab.as_ref());
            // Attribution over every positive of the split (incl. non-primary populations).
            let all_idx = observed(sc, &all_rows);
            let all_owned = scores_of(sc, &all_idx);
            let all_s: Vec<&Score> = all_owned.iter().collect();
            m["attribution_all_positives"] = attribution(rows, &all_idx, &all_s, sc.vocab.as_ref());
            let all_held: Vec<usize> = (0..all_idx.len())
                .filter(|&k| rows[all_idx[k]].held_out_style)
                .collect();
            m["attribution_all_positives_held_out_voices"] = attribution(
                rows,
                &all_held.iter().map(|&k| all_idx[k]).collect::<Vec<_>>(),
                &all_held.iter().map(|&k| all_s[k]).collect::<Vec<_>>(),
                sc.vocab.as_ref(),
            );
            m["predictions_failed_or_missing"] = json!(sc.failures);
            // Slices.
            let mut sl = Map::new();
            for (slc, st) in slices.iter().zip(&status) {
                if pi.is_some() && slc.non_primary {
                    continue;
                }
                let idx = observed(sc, &slc.idx);
                if idx.is_empty() {
                    continue;
                }
                let owned = scores_of(sc, &idx);
                let s: Vec<&Score> = owned.iter().collect();
                let ci = !opts.no_interval_dimensions.contains(&slc.dimension);
                let mut v = detection(rows, &slc.idx, &idx, &s, pi, &sc.ops, ci, true);
                if let Some(o) = v.as_object_mut() {
                    o.remove("calibration_bins");
                    o.insert("kind".into(), json!(slc.kind));
                    o.insert(
                        "below_slice_minimum".into(),
                        st["below_slice_minimum"].clone(),
                    );
                }
                if slc.kind != "humans" && sc.vocab.is_some() {
                    let a = attribution(rows, &idx, &s, sc.vocab.as_ref());
                    v["attribution_accuracy"] = a["accuracy"].clone();
                    v["attribution_macro_f1"] = a["macro_f1"].clone();
                    if slc.dimension.starts_with("frontier") {
                        v["frontier_attribution"] =
                            frontier_attribution(rows, &idx, &s, sc.vocab.as_ref(), &frontier);
                    }
                }
                sl.entry(slc.dimension.clone()).or_insert_with(|| json!({}))[&slc.value] = v;
            }
            m["slices"] = Value::Object(sl);
            per_seed.push(m.clone());
            seeds_json.insert(sc.seed.to_string(), m);
        }
        // Recipe: seed means, seed-averaged per-document losses for the interval,
        // and the largest flag counts.
        let mut recipe = Map::new();
        let common: Vec<usize> = primary
            .iter()
            .copied()
            .filter(|&i| ss.iter().all(|sc| sc.scores[i].is_some()))
            .collect();
        let avg_loss: Vec<f64> = common
            .iter()
            .map(|&i| {
                ss.iter()
                    .map(|sc| sc.scores[i].as_ref().unwrap().loss(&rows[i]))
                    .sum::<f64>()
                    / ss.len() as f64
            })
            .collect();
        let wb = est_weights(rows, &primary, &common, pi, false);
        recipe.insert(
            "seeds".into(),
            json!(ss.iter().map(|s| s.seed).collect::<Vec<_>>()),
        );
        recipe.insert(
            "balanced_involvement_brier".into(),
            json!((!common.is_empty()).then(|| wmean(&wb, &avg_loss))),
        );
        recipe.insert(
            "balanced_involvement_brier_95ci".into(),
            ci_json(bootstrap_interval(&group_values(
                rows, &primary, &common, &wb, &avg_loss,
            ))),
        );
        for (key, ptr) in [
            (
                "family_weighted_involvement_brier",
                "/family_weighted_involvement_brier",
            ),
            ("human_fpr_at_0_5", "/human_fpr_at_0_5/rate"),
            ("tpr_at_0_5", "/tpr_at_0_5/rate"),
            ("tpr_at_fpr_0.01", "/operating_points/0.01/tpr/rate"),
            ("tpr_at_fpr_0.001", "/operating_points/0.001/tpr/rate"),
            (
                "human_fpr_at_fpr_0.01",
                "/operating_points/0.01/human_fpr/rate",
            ),
            (
                "human_fpr_at_fpr_0.001",
                "/operating_points/0.001/human_fpr/rate",
            ),
            ("ece10", "/ece10"),
            ("ece10_humans", "/ece10_humans"),
            ("share_mae_points", "/share/overall/mae_points"),
            (
                "share_mse_points_squared",
                "/share/overall/mse_points_squared",
            ),
            ("share_mixed_mae_points", "/share/mixed/mae_points"),
            (
                "three_way_balanced_brier_sum3",
                "/three_way/balanced_brier_sum3",
            ),
            (
                "three_way_balanced_accuracy_weighted",
                "/three_way/balanced_accuracy_weighted",
            ),
            ("three_way_macro_f1", "/three_way/macro_f1_unweighted"),
            ("attribution_accuracy", "/attribution/accuracy"),
            ("attribution_macro_f1", "/attribution/macro_f1"),
            (
                "attribution_held_out_voices_accuracy",
                "/attribution_held_out_voices/accuracy",
            ),
            (
                "attribution_held_out_voices_macro_f1",
                "/attribution_held_out_voices/macro_f1",
            ),
            (
                "frontier_attribution_held_out_voices_accuracy",
                "/frontier_attribution/held_out_voices/accuracy",
            ),
            (
                "frontier_attribution_all_voices_accuracy",
                "/frontier_attribution/all_voices/accuracy",
            ),
            (
                "human_role_attribution_accuracy",
                "/role_attribution/human/accuracy",
            ),
            (
                "ai_role_attribution_accuracy",
                "/role_attribution/ai/accuracy",
            ),
            (
                "attribution_all_positives_accuracy",
                "/attribution_all_positives/accuracy",
            ),
            (
                "attribution_all_positives_held_out_voices_accuracy",
                "/attribution_all_positives_held_out_voices/accuracy",
            ),
        ] {
            let v: Vec<f64> = per_seed.iter().filter_map(|m| get_f(m, ptr)).collect();
            recipe.insert(
                format!("{key}_seed_mean"),
                json!(if v.len() == per_seed.len() {
                    mean(&v)
                } else {
                    None
                }),
            );
        }
        for (key, ptr) in [
            ("human_flags_at_0_5_max", "/human_fpr_at_0_5/count"),
            (
                "human_flags_at_fpr_0.01_max",
                "/operating_points/0.01/human_fpr/count",
            ),
            (
                "human_flags_at_fpr_0.001_max",
                "/operating_points/0.001/human_fpr/count",
            ),
        ] {
            let v: Vec<u64> = per_seed
                .iter()
                .filter_map(|m| m.pointer(ptr).and_then(Value::as_u64))
                .collect();
            recipe.insert(key.into(), json!(v.iter().max()));
        }
        let mut rs = Map::new();
        for slc in &slices {
            let v: Vec<f64> = per_seed
                .iter()
                .filter_map(|m| {
                    get_f(
                        m,
                        &format!(
                            "/slices/{}/{}/balanced_involvement_brier",
                            esc(&slc.dimension),
                            esc(&slc.value)
                        ),
                    )
                })
                .collect();
            let f: Vec<u64> = per_seed
                .iter()
                .filter_map(|m| {
                    m.pointer(&format!(
                        "/slices/{}/{}/human_fpr_at_0_5/count",
                        esc(&slc.dimension),
                        esc(&slc.value)
                    ))
                    .and_then(Value::as_u64)
                })
                .collect();
            if v.len() == per_seed.len() {
                rs.entry(slc.dimension.clone()).or_insert_with(|| json!({}))[&slc.value] = json!({"balanced_involvement_brier_seed_mean": mean(&v), "human_flags_at_0_5_max": f.iter().max()});
            }
        }
        recipe.insert("slices".into(), Value::Object(rs));
        let released = ss
            .iter()
            .find(|s| s.seed == opts.released_seed)
            .or(if ss.len() == 1 { ss.first() } else { None });
        // Missing comparator observations (protocol: more than 2% of the
        // selected primary documents missing means Tier C is not evaluated).
        let subsample_missing = sys.subsample.as_ref().map(|sub| {
            let n = sub.len();
            let worst = ss.iter().map(|s| s.failures).max().unwrap_or(0);
            json!({"selected": n, "missing_max_over_seeds": worst, "missing_fraction": if n > 0 { worst as f64 / n as f64 } else { 0.0 }, "exceeds_2pct": worst * 50 > n})
        });
        systems_json.insert(
            sys.name.clone(),
            json!({
                "missing_rule": match sys.missing { Missing::Worst => "worst_case_execution_failure", Missing::Best => "best_case_missing_comparator" },
                "population": if sys.subsample.is_some() { "pre_registered_subsample_horvitz_thompson" } else { "full_split" },
                "subsample_documents": sys.subsample.as_ref().map(|s| s.len()),
                "subsample_missing": subsample_missing,
                "released_seed": released.map(|s| s.seed),
                "seeds": seeds_json,
                "recipe": recipe,
            }),
        );
    }

    // Paired comparisons.
    let mut comps = vec![];
    for (a, b) in comparisons {
        let sa = scored
            .get(a)
            .with_context(|| format!("comparison names unknown system {a}"))?;
        let sb = scored
            .get(b)
            .with_context(|| format!("comparison names unknown system {b}"))?;
        let ia = systems.iter().find(|s| &s.name == a).unwrap();
        let ib = systems.iter().find(|s| &s.name == b).unwrap();
        let sub = match (&ia.subsample, &ib.subsample) {
            (None, None) => None,
            (Some(_), None) => Some(a),
            (None, Some(_)) => Some(b),
            (Some(_), Some(_)) => bail!("comparison {a} vs {b}: both sides are subsamples"),
        };
        let paired = Paired {
            rows,
            pi: sub.map(|n| incl[n].clone()),
        };
        let pick = |ss: &'_ [Scored]| -> Option<usize> {
            ss.iter()
                .position(|s| s.seed == opts.released_seed)
                .or(if ss.len() == 1 { Some(0) } else { None })
        };
        let loss = |ss: &[Scored], i: usize, which: Option<usize>| -> f64 {
            match which {
                Some(k) => ss[k].scores[i].as_ref().unwrap().loss(&rows[i]),
                None => {
                    ss.iter()
                        .map(|s| s.scores[i].as_ref().unwrap().loss(&rows[i]))
                        .sum::<f64>()
                        / ss.len() as f64
                }
            }
        };
        let err = |ss: &[Scored], i: usize, which: Option<usize>| -> f64 {
            let e =
                |s: &Scored| (s.scores[i].as_ref().unwrap().share - rows[i].share).abs() * 100.0;
            match which {
                Some(k) => e(&ss[k]),
                None => ss.iter().map(e).sum::<f64>() / ss.len() as f64,
            }
        };
        let both = |i: usize| sa[0].scores[i].is_some() && sb[0].scores[i].is_some();
        let mut variants = Map::new();
        for (vname, wa, wb) in [("recipe", None, None), ("released", pick(sa), pick(sb))] {
            if vname == "released" && (wa.is_none() || wb.is_none()) {
                variants.insert(vname.into(), Value::Null);
                continue;
            }
            let compute = |idx: &[usize], ci: bool| -> Result<Value> {
                // Full mode: the documents both systems observed. HT mode: the
                // slice's own primary documents, with unsampled ones contributing 0.
                let idx: Vec<usize> = match &paired.pi {
                    None => idx.iter().copied().filter(|&i| both(i)).collect(),
                    Some(_) => idx.iter().copied().filter(|&i| rows[i].primary).collect(),
                };
                if idx.is_empty() || (paired.pi.is_some() && !idx.iter().any(|&i| both(i))) {
                    return Ok(Value::Null);
                }
                let w = balanced_weights(rows, &idx);
                let wf = family_weights(rows, &idx);
                let d = |i: usize| {
                    if both(i) {
                        loss(sa, i, wa) - loss(sb, i, wb)
                    } else {
                        0.0
                    }
                };
                let de = |i: usize| {
                    if both(i) {
                        err(sa, i, wa) - err(sb, i, wb)
                    } else {
                        0.0
                    }
                };
                let v = paired.values(&idx, &d, &w)?;
                let vf = paired.values(&idx, &d, &wf)?;
                let vs = paired.values(&idx, &de, &w)?;
                let iv = |v: &[f64]| {
                    if ci {
                        ci_json(bootstrap_interval(v))
                    } else {
                        Value::Null
                    }
                };
                Ok(json!({
                    "documents": idx.len(),
                    "source_groups": v.len(),
                    "estimator": if paired.pi.is_some() { "horvitz_thompson_over_subsample" } else { "full_population" },
                    "balanced_brier_difference": mean(&v),
                    "balanced_brier_difference_95ci": iv(&v),
                    "family_weighted_brier_difference": mean(&vf),
                    "family_weighted_brier_difference_95ci": iv(&vf),
                    "share_mae_points_difference": mean(&vs),
                    "share_mae_points_difference_95ci": iv(&vs),
                }))
            };
            let mut v = compute(&primary, true)?;
            let mut sl = Map::new();
            for slc in &slices {
                if paired.pi.is_some() && slc.non_primary {
                    continue;
                }
                let ci = !opts.no_interval_dimensions.contains(&slc.dimension);
                let r = compute(&slc.idx, ci)?;
                if !r.is_null() {
                    sl.entry(slc.dimension.clone()).or_insert_with(|| json!({}))[&slc.value] = r;
                }
            }
            // The protocol's gated slices: realised status and the gap point estimate.
            let g: Vec<Value> = gated
                .iter()
                .map(|gs| {
                    let r = sl
                        .get(gs["dimension"].as_str().unwrap())
                        .and_then(|m| m.get(gs["value"].as_str().unwrap()));
                    json!({
                        "slice": gs["slice"],
                        "status": gs["status"],
                        "balanced_brier_difference": r.map(|x| x["balanced_brier_difference"].clone()),
                        "balanced_brier_difference_95ci": r.map(|x| x["balanced_brier_difference_95ci"].clone()),
                    })
                })
                .collect();
            v["gated_slices"] = json!(g);
            v["slices"] = Value::Object(sl);
            v["seed_a"] = json!(wa.map(|k| sa[k].seed));
            v["seed_b"] = json!(wb.map(|k| sb[k].seed));
            variants.insert(vname.into(), v);
        }
        comps.push(json!({"a": a, "b": b, "difference": "a_minus_b", "subsample_side": sub, "recipe_rule": "per_document_losses_averaged_over_each_side's_seeds_before_group_resampling", "variants": variants}));
    }

    let mut counts: BTreeMap<String, BTreeMap<String, usize>> = BTreeMap::new();
    let mut by_population: BTreeMap<String, BTreeMap<String, usize>> = BTreeMap::new();
    for r in rows {
        *counts
            .entry(r.domain.clone())
            .or_default()
            .entry(r.cell.name().into())
            .or_default() += 1;
        *by_population
            .entry(r.population.clone())
            .or_default()
            .entry(r.cell.name().into())
            .or_default() += 1;
    }
    Ok(json!({
        "schema": EVAL_SCHEMA,
        "protocol": "docs/a-competitive-protocol.md",
        "split": split,
        "population": {
            "documents": rows.len(),
            "primary_documents": primary.len(),
            "primary_populations": PRIMARY_POPULATIONS,
            "source_groups": rows.iter().map(|r| r.group.as_str()).collect::<BTreeSet<_>>().len(),
            "by_domain_and_origin": counts,
            "by_population_and_origin": by_population,
            "frontier_labels": frontier,
            "frontier_labels_source": frontier_source,
            "slices": slices.len(),
        },
        "gated_slices": gated,
        "systems": systems_json,
        "comparisons": comps,
    }))
}

/// JSON-pointer escape.
pub fn esc(s: &str) -> String {
    s.replace('~', "~0").replace('/', "~1")
}

// ---------------------------------------------------------------- markdown

fn f(v: Option<&Value>, digits: usize) -> String {
    match v.and_then(Value::as_f64) {
        Some(x) => format!("{x:.digits$}"),
        None => "—".into(),
    }
}

fn ci(v: Option<&Value>) -> String {
    match v {
        Some(c) if !c.is_null() => format!("[{}, {}]", f(c.get("lower"), 5), f(c.get("upper"), 5)),
        _ => "—".into(),
    }
}

fn kn(v: Option<&Value>) -> String {
    match v {
        Some(r) if !r.is_null() => format!(
            "{}/{} (CP95 ≤ {})",
            r["count"],
            r["n"],
            f(r.get("clopper_pearson_upper_one_sided_95"), 4)
        ),
        _ => "—".into(),
    }
}

/// Markdown summary tables for the evaluation JSON.
pub fn markdown(e: &Value) -> String {
    let mut s = String::new();
    let p = &e["population"];
    s += &format!(
        "# Competitive A evaluation: split `{}`\n\nSchema `{}`. {} documents ({} primary) in {} source groups.\n\n",
        e["split"].as_str().unwrap_or(""),
        EVAL_SCHEMA,
        p["documents"],
        p["primary_documents"],
        p["source_groups"]
    );
    s += "## Primary population, per seed\n\n| System | Seed | Balanced Brier [95% CI] | Family Brier | Human FPR @0.5 | Human FPR @1% op | TPR @1% op | TPR @0.1% op | ECE10 / humans | Share MAE mixed (pp) | 3-way acc | Attribution acc / macro-F1 | Held-out voices acc / macro-F1 | Failures |\n|---|---|---|---|---|---|---|---|---|---|---|---|---|---|\n";
    let empty = Map::new();
    let systems = e["systems"].as_object().unwrap_or(&empty);
    for (name, sys) in systems {
        for (seed, m) in sys["seeds"].as_object().unwrap_or(&empty) {
            let op = |a: &str, k: &str| m.pointer(&format!("/operating_points/{a}/{k}")).cloned();
            s += &format!(
                "| {name} | {seed} | {} {} | {} | {} | {} | {} | {} | {} / {} | {} | {} | {} / {} | {} / {} | {} |\n",
                f(m.get("balanced_involvement_brier"), 5),
                ci(m.get("balanced_involvement_brier_95ci")),
                f(m.get("family_weighted_involvement_brier"), 5),
                kn(m.get("human_fpr_at_0_5")),
                kn(op("0.01", "human_fpr").as_ref()),
                f(op("0.01", "tpr").as_ref().and_then(|v| v.get("rate")), 4),
                f(op("0.001", "tpr").as_ref().and_then(|v| v.get("rate")), 4),
                f(m.get("ece10"), 4),
                f(m.get("ece10_humans"), 4),
                f(m.pointer("/share/mixed/mae_points"), 2),
                f(m.pointer("/three_way/balanced_accuracy_weighted"), 4),
                f(m.pointer("/attribution/accuracy"), 4),
                f(m.pointer("/attribution/macro_f1"), 4),
                f(m.pointer("/attribution_held_out_voices/accuracy"), 4),
                f(m.pointer("/attribution_held_out_voices/macro_f1"), 4),
                m["execution_failures"],
            );
        }
    }
    if let Some(g) = e["gated_slices"].as_array() {
        s += "\n## Gated slices (realised minimum)\n\n| Slice | Status | Groups | Humans | Frontier positives | Shortfall |\n|---|---|---|---|---|---|\n";
        for x in g {
            let r = &x["realised"];
            let short = r
                .get("shortfall")
                .or(x.get("shortfall"))
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join("; ")
                })
                .unwrap_or_default();
            s += &format!(
                "| {} | {} | {} | {} | {} | {} |\n",
                x["slice"].as_str().unwrap_or(""),
                x["status"].as_str().unwrap_or(""),
                r["source_groups"],
                r["human_documents"],
                r["frontier_positives"],
                short
            );
        }
    }
    s += "\n## Frontier and role attribution, per seed\n\n| System | Seed | Frontier top-1 (held-out voices) | Frontier top-1 (all voices) | Human role acc | AI role acc | Human role involvement Brier |\n|---|---|---|---|---|---|---|\n";
    for (name, sys) in systems {
        for (seed, m) in sys["seeds"].as_object().unwrap_or(&empty) {
            let fa = |p: &str| {
                let d = m.pointer(&format!("/frontier_attribution/{p}"));
                match d {
                    Some(d) if d.get("accuracy").is_some() => {
                        format!("{} ({})", f(d.get("accuracy"), 4), d["documents"])
                    }
                    _ => "—".into(),
                }
            };
            s += &format!(
                "| {name} | {seed} | {} | {} | {} | {} | {} |\n",
                fa("held_out_voices"),
                fa("all_voices"),
                f(m.pointer("/role_attribution/human/accuracy"), 4),
                f(m.pointer("/role_attribution/ai/accuracy"), 4),
                f(
                    m.pointer("/role_attribution/human/role_involvement_balanced_brier"),
                    5
                ),
            );
        }
    }
    s += "\n## Recipe (seed mean)\n\n| System | Seeds | Balanced Brier [95% CI, seed-averaged] | Family Brier | Max human flags @0.5 / @1% op | TPR @1% / @0.1% op | ECE10 | Share MAE mixed (pp) | Attribution acc (held-out voices) |\n|---|---|---|---|---|---|---|---|---|\n";
    for (name, sys) in systems {
        let r = &sys["recipe"];
        s += &format!(
            "| {name} | {} | {} {} | {} | {} / {} | {} / {} | {} | {} | {} ({}) |\n",
            r["seeds"],
            f(r.get("balanced_involvement_brier"), 5),
            ci(r.get("balanced_involvement_brier_95ci")),
            f(r.get("family_weighted_involvement_brier_seed_mean"), 5),
            r["human_flags_at_0_5_max"],
            r["human_flags_at_fpr_0.01_max"],
            f(r.get("tpr_at_fpr_0.01_seed_mean"), 4),
            f(r.get("tpr_at_fpr_0.001_seed_mean"), 4),
            f(r.get("ece10_seed_mean"), 4),
            f(r.get("share_mixed_mae_points_seed_mean"), 2),
            f(r.get("attribution_accuracy_seed_mean"), 4),
            f(r.get("attribution_held_out_voices_accuracy_seed_mean"), 4),
        );
    }
    if let Some(cs) = e["comparisons"].as_array().filter(|c| !c.is_empty()) {
        s += "\n## Paired differences (A minus B)\n\n| A | B | Variant | Estimator | Balanced Brier diff [95% CI] | Family Brier diff [95% CI] | Share MAE diff (pp) [95% CI] |\n|---|---|---|---|---|---|---|\n";
        for c in cs {
            for (vn, v) in c["variants"].as_object().unwrap_or(&empty) {
                if v.is_null() {
                    continue;
                }
                s += &format!(
                    "| {} | {} | {vn} | {} | {} {} | {} {} | {} {} |\n",
                    c["a"].as_str().unwrap_or(""),
                    c["b"].as_str().unwrap_or(""),
                    v["estimator"].as_str().unwrap_or(""),
                    f(v.get("balanced_brier_difference"), 5),
                    ci(v.get("balanced_brier_difference_95ci")),
                    f(v.get("family_weighted_brier_difference"), 5),
                    ci(v.get("family_weighted_brier_difference_95ci")),
                    f(v.get("share_mae_points_difference"), 2),
                    ci(v.get("share_mae_points_difference_95ci")),
                );
            }
        }
        s += "\n### Gated slice differences (recipe)\n\n| A | B | Slice | Status | Diff [95% CI] |\n|---|---|---|---|---|\n";
        for c in cs {
            for g in c
                .pointer("/variants/recipe/gated_slices")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                s += &format!(
                    "| {} | {} | {} | {} | {} {} |\n",
                    c["a"].as_str().unwrap_or(""),
                    c["b"].as_str().unwrap_or(""),
                    g["slice"].as_str().unwrap_or(""),
                    g["status"].as_str().unwrap_or(""),
                    f(g.get("balanced_brier_difference"), 5),
                    ci(g.get("balanced_brier_difference_95ci")),
                );
            }
        }
        s += "\n### Slice differences (recipe)\n\n| A | B | Dimension | Value | Docs | Groups | Diff [95% CI] |\n|---|---|---|---|---|---|---|\n";
        for c in cs {
            let Some(sl) = c
                .pointer("/variants/recipe/slices")
                .and_then(Value::as_object)
            else {
                continue;
            };
            for (d, vals) in sl {
                if [
                    "voice_cluster",
                    "human_style_axis",
                    "held_out_voice_cluster",
                ]
                .contains(&d.as_str())
                {
                    continue;
                }
                for (k, v) in vals.as_object().unwrap_or(&empty) {
                    s += &format!(
                        "| {} | {} | {d} | {k} | {} | {} | {} {} |\n",
                        c["a"].as_str().unwrap_or(""),
                        c["b"].as_str().unwrap_or(""),
                        v["documents"],
                        v["source_groups"],
                        f(v.get("balanced_brier_difference"), 5),
                        ci(v.get("balanced_brier_difference_95ci")),
                    );
                }
            }
        }
    }
    // Per-system slice tables (released or first seed).
    for (name, sys) in systems {
        let seed = sys["released_seed"]
            .as_u64()
            .map(|x| x.to_string())
            .or_else(|| {
                sys["seeds"]
                    .as_object()
                    .and_then(|m| m.keys().next().cloned())
            });
        let Some(seed) = seed else { continue };
        let Some(sl) = sys
            .pointer(&format!("/seeds/{seed}/slices"))
            .and_then(Value::as_object)
        else {
            continue;
        };
        s += &format!(
            "\n## Slices for {name}, seed {seed}\n\n| Dimension | Value | Kind | Docs | Groups | Humans | Balanced Brier [95% CI] | Human FPR @0.5 | TPR @0.5 | Attribution acc | Below minimum |\n|---|---|---|---|---|---|---|---|---|---|---|\n"
        );
        for (d, vals) in sl {
            let vals = vals.as_object().unwrap();
            if vals.len() > 40 {
                let mut b: Vec<f64> = vals
                    .values()
                    .filter_map(|v| v["balanced_involvement_brier"].as_f64())
                    .collect();
                b.sort_by(f64::total_cmp);
                if b.is_empty() {
                    continue;
                }
                let q = |p: f64| {
                    b.get(((b.len() - 1) as f64 * p).round() as usize)
                        .copied()
                        .unwrap_or(f64::NAN)
                };
                s += &format!(
                    "| {d} | {} values (balanced Brier p10 / p50 / p90 / max: {:.5} / {:.5} / {:.5} / {:.5}) | — | — | — | — | see JSON | — | — | — | — |\n",
                    vals.len(),
                    q(0.1),
                    q(0.5),
                    q(0.9),
                    q(1.0)
                );
                continue;
            }
            for (k, v) in vals {
                s += &format!(
                    "| {d} | {k} | {} | {} | {} | {} | {} {} | {} | {} | {} | {} |\n",
                    v["kind"].as_str().unwrap_or(""),
                    v["documents"],
                    v["source_groups"],
                    v["origin_counts"]["human_only"],
                    f(v.get("balanced_involvement_brier"), 5),
                    ci(v.get("balanced_involvement_brier_95ci")),
                    kn(v.get("human_fpr_at_0_5")),
                    f(v.pointer("/tpr_at_0_5/rate"), 4),
                    f(v.get("attribution_accuracy"), 4),
                    v["below_slice_minimum"],
                );
            }
        }
        if let Some(sh) = sys
            .pointer(&format!("/seeds/{seed}/share/by_realised_share"))
            .and_then(Value::as_object)
        {
            s += &format!(
                "\n### Share by realised-share bin, {name} seed {seed}\n\n| Bin | Docs | MAE (pp) | MSE (pp²) | Bias (pp) |\n|---|---|---|---|---|\n"
            );
            for (k, v) in sh {
                s += &format!(
                    "| {k} | {} | {} | {} | {} |\n",
                    v["documents"],
                    f(v.get("mae_points"), 2),
                    f(v.get("mse_points_squared"), 1),
                    f(v.get("bias_points"), 2)
                );
            }
        }
        if let Some(pg) = sys
            .pointer(&format!(
                "/seeds/{seed}/attribution_all_positives/per_generator"
            ))
            .and_then(Value::as_object)
        {
            s += &format!(
                "\n### Attribution per generator (all positives), {name} seed {seed}\n\n| Generator | Docs | Accuracy | Target |\n|---|---|---|---|\n"
            );
            for (k, v) in pg {
                s += &format!(
                    "| {k} | {} | {} | {} |\n",
                    v["documents"],
                    f(v.get("accuracy"), 4),
                    v["target"].as_str().unwrap_or("")
                );
            }
        }
    }
    s
}

// ---------------------------------------------------------------- tests

#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: &str, g: &str, origin: &str, share: f64) -> Row {
        row_in(id, g, origin, share, "primary")
    }

    fn row_in(id: &str, g: &str, origin: &str, share: f64, population: &str) -> Row {
        let label = (origin != "human_only").then(|| "gen-a".to_string());
        let op = if population.starts_with("robustness") {
            "continuation"
        } else {
            "mirror"
        };
        Row::from_raw(RawRow {
            id: id.into(),
            split: "dev_iter".into(),
            origin: origin.into(),
            share,
            spans: None,
            generator_label: label,
            generator_family: None,
            cohort_id: None,
            cohort_weight: None,
            source_group: g.into(),
            domain: "wikipedia".into(),
            genre: None,
            voice_id: None,
            voice_cluster: None,
            voice_origin: None,
            style_axes: None,
            operation: (origin != "human_only").then(|| op.to_string()),
            held_out_style: None,
            population: population.into(),
            voice_kind: None,
            ocr_tier: None,
            removed_markup_per_100_words: None,
            markup_removed: None,
            demographics: None,
            human_attribution_label: None,
        })
        .unwrap()
    }

    fn pred(inv: f64) -> Obs {
        Obs::Ok(Prediction {
            p_human: 1.0 - inv,
            p_model_only: inv,
            p_mixed: 0.0,
            share: inv,
            attribution: BTreeMap::new(),
            roles: BTreeMap::new(),
        })
    }

    fn sys(name: &str, seeds: Vec<(u64, Vec<(&str, f64)>)>) -> SystemInput {
        SystemInput {
            name: name.into(),
            seeds: seeds
                .into_iter()
                .map(|(seed, v)| SeedInput {
                    seed,
                    predictions: v
                        .into_iter()
                        .map(|(i, p)| (i.to_string(), pred(p)))
                        .collect(),
                    operating_points: None,
                })
                .collect(),
            subsample: None,
            missing: Missing::Worst,
        }
    }

    fn approx(a: f64, b: f64) {
        assert!((a - b).abs() < 1e-12, "{a} != {b}");
    }

    fn fixture() -> Vec<Row> {
        vec![
            row("h1", "g1", "human_only", 0.0),
            row("x1", "g1", "mixed", 0.4),
            row("m1", "g1", "model_only", 1.0),
            row("h2", "g2", "human_only", 0.0),
            row("m2", "g2", "model_only", 1.0),
            row("m3", "g2", "model_only", 1.0),
        ]
    }

    fn preds() -> Vec<(&'static str, f64)> {
        vec![
            ("h1", 0.2),
            ("x1", 0.6),
            ("m1", 1.0),
            ("h2", 0.0),
            ("m2", 0.5),
            ("m3", 0.7),
        ]
    }

    #[test]
    fn balanced_and_family_brier_match_hand_computation() {
        let rows = fixture();
        let e = evaluate(
            &rows,
            &[sys("A", vec![(17, preds())])],
            &[],
            &Options::default(),
        )
        .unwrap();
        let m = &e["systems"]["A"]["seeds"]["17"];
        // g1: cells h 0.04, mixed 0.16, model 0 -> 0.2/3; g2: h 0, model (0.25+0.09)/2.
        let bal = (0.2 / 3.0 + (0.0 + 0.17) / 2.0) / 2.0;
        approx(m["balanced_involvement_brier"].as_f64().unwrap(), bal);
        let fam = (0.2 / 3.0 + 0.34 / 3.0) / 2.0;
        approx(
            m["family_weighted_involvement_brier"].as_f64().unwrap(),
            fam,
        );
        approx(
            e["systems"]["A"]["recipe"]["balanced_involvement_brier"]
                .as_f64()
                .unwrap(),
            bal,
        );
        // Two groups: the CI exists and brackets the point estimate.
        let c = &m["balanced_involvement_brier_95ci"];
        assert!(c["lower"].as_f64().unwrap() <= bal && bal <= c["upper"].as_f64().unwrap());
        // Positives at 0.5: x1 0.6, m1 1.0, m2 0.5, m3 0.7 -> all flagged; humans none.
        assert_eq!(m["tpr_at_0_5"]["count"], 4);
        assert_eq!(m["human_fpr_at_0_5"]["count"], 0);
        approx(
            m["human_fpr_at_0_5"]["clopper_pearson_upper_one_sided_95"]
                .as_f64()
                .unwrap(),
            1.0 - 0.05f64.powf(0.5),
        );
    }

    #[test]
    fn ece10_share_and_three_way_are_exact() {
        // Four documents in four groups, one cell each: equal weights 1/4.
        let rows = vec![
            row("h1", "g1", "human_only", 0.0),
            row("h2", "g2", "human_only", 0.0),
            row("m1", "g3", "model_only", 1.0),
            row("x1", "g4", "mixed", 0.3),
        ];
        let mut s = sys(
            "A",
            vec![(
                17,
                vec![("h1", 0.25), ("h2", 0.25), ("m1", 0.25), ("x1", 0.25)],
            )],
        );
        // Make x1 a mixed prediction with share 0.5 to test three-way and share.
        s.seeds[0].predictions.insert(
            "x1".into(),
            Obs::Ok(Prediction {
                p_human: 0.75,
                p_model_only: 0.0,
                p_mixed: 0.25,
                share: 0.5,
                attribution: BTreeMap::new(),
                roles: BTreeMap::new(),
            }),
        );
        let e = evaluate(&rows, &[s], &[], &Options::default()).unwrap();
        let m = &e["systems"]["A"]["seeds"]["17"];
        // All in bin 2: mean p 0.25, observed 0.5.
        approx(m["ece10"].as_f64().unwrap(), 0.25);
        approx(m["ece10_humans"].as_f64().unwrap(), 0.25);
        // Share errors (pp): h 25, 25; m1 75; x1 20 -> MAE 36.25.
        approx(m["share"]["overall"]["mae_points"].as_f64().unwrap(), 36.25);
        approx(
            m["share"]["overall"]["mse_points_squared"]
                .as_f64()
                .unwrap(),
            (625.0 * 2.0 + 5625.0 + 400.0) / 4.0,
        );
        approx(m["share"]["mixed"]["mae_points"].as_f64().unwrap(), 20.0);
        assert_eq!(
            m["share"]["by_realised_share"]["mixed:[25,50)"]["documents"],
            1
        );
        // Three-way: all predicted human -> accuracy 1/2; macro-F1 over 3 classes:
        // human P=0.5 R=1 F=2/3; others 0 -> 2/9.
        let t = &m["three_way"];
        approx(t["balanced_accuracy_weighted"].as_f64().unwrap(), 0.5);
        approx(t["macro_f1_unweighted"].as_f64().unwrap(), 2.0 / 9.0);
        // Brier3: h: .0625+.0625=.125; m1 [.75,.25,0]: .5625+.5625=1.125; x1 [.75,0,.25]: .5625+.5625=1.125.
        approx(
            t["balanced_brier_sum3"].as_f64().unwrap(),
            (0.125 * 2.0 + 1.125 * 2.0) / 4.0,
        );
        assert_eq!(
            t["confusion_rows_true_cols_predicted"],
            json!([[2, 0, 0], [1, 0, 0], [1, 0, 0]])
        );
    }

    #[test]
    fn operating_point_reads_emitted_involvement_not_resummed_split() {
        // One isotonic level p; the model-only/mixed split of p re-adds to p + 1 ulp
        // for r = 0.0015 and to p for r = 0.5. Both documents sit at the same level.
        let p = f64::from_bits(0x3febc1670cb257fd);
        let split = |r: f64| {
            Obs::Ok(Prediction {
                p_human: 1.0 - p,
                p_model_only: p * r,
                p_mixed: p * (1.0 - r),
                share: p * r,
                attribution: BTreeMap::new(),
                roles: BTreeMap::new(),
            })
        };
        let frontier = BTreeSet::new();
        let h = row("h", "g1", "human_only", 0.0);
        let a = score_obs(&h, Some(&split(0.0015)), Missing::Worst, &frontier);
        let b = score_obs(&h, Some(&split(0.5)), Missing::Worst, &frontier);
        assert_ne!(a.inv.to_bits(), b.inv.to_bits());
        assert_eq!(a.flag.to_bits(), p.to_bits());
        assert_eq!(b.flag.to_bits(), p.to_bits());
        // Two humans tied at the level: a 1% point over 100 humans cannot split them.
        let mut hum: Vec<f64> = vec![0.1; 98];
        hum.extend([a.flag, b.flag]);
        let next = 0.95;
        let mut all = hum.clone();
        all.push(next);
        assert_eq!(operating_threshold(&all, &hum, ALPHA_1PCT), next);
        let mut noisy: Vec<f64> = vec![0.1; 98];
        noisy.extend([a.inv, b.inv]);
        let mut all_noisy = noisy.clone();
        all_noisy.push(next);
        // The re-summed values would split the tie (the defect being fixed).
        assert_eq!(operating_threshold(&all_noisy, &noisy, ALPHA_1PCT), a.inv.max(b.inv));
    }

    #[test]
    fn operating_threshold_follows_protocol_rule() {
        let h: Vec<f64> = (1..=100).map(|i| i as f64 / 100.0).collect();
        let mut all = h.clone();
        all.push(0.995);
        // At most 1 of 100 humans >= t: t = 1.0 flags exactly one (1.0); 0.995 flags one too.
        approx(operating_threshold(&all, &h, ALPHA_1PCT), 0.995);
        // 0.1%: zero humans allowed -> smallest candidate above every human = +inf here.
        assert_eq!(operating_threshold(&all, &h, ALPHA_01PCT), f64::INFINITY);
        all.push(1.5);
        approx(operating_threshold(&all, &h, ALPHA_01PCT), 1.5);
        // Ties count as flagged.
        let h = vec![0.9, 0.9];
        assert_eq!(operating_threshold(&[0.9], &h, ALPHA_1PCT), f64::INFINITY);
        assert_eq!(
            parse_threshold(&json!({"bits": format!("0x{:016x}", 0.3f64.to_bits())})).unwrap(),
            0.3
        );
        assert_eq!(parse_threshold(&json!("inf")).unwrap(), f64::INFINITY);
        assert!(parse_threshold(&json!({"bits": "0x3fd3333333333333", "value": 0.31})).is_err());
    }

    #[test]
    fn declared_operating_points_give_fpr_and_tpr() {
        let rows = fixture();
        let mut s = sys("A", vec![(17, preds())]);
        s.seeds[0].operating_points =
            Some(BTreeMap::from([("0.01", 0.65), ("0.001", f64::INFINITY)]));
        let e = evaluate(&rows, &[s], &[], &Options::default()).unwrap();
        let op = &e["systems"]["A"]["seeds"]["17"]["operating_points"];
        assert_eq!(op["0.01"]["tpr"]["count"], 2); // m1 1.0, m3 0.7
        assert_eq!(op["0.01"]["human_fpr"]["count"], 0);
        assert_eq!(op["0.001"]["tpr"]["count"], 0);
        assert_eq!(op["0.01"]["threshold"]["source"], "declared");
    }

    #[test]
    fn failures_and_missing_rows_score_worst_case() {
        let rows = fixture();
        let mut p = preds();
        p.retain(|(i, _)| *i != "h1");
        let mut s = sys("A", vec![(17, p)]);
        s.seeds[0]
            .predictions
            .insert("m1".into(), Obs::Failed("timeout".into()));
        let e = evaluate(&rows, &[s], &[], &Options::default()).unwrap();
        let m = &e["systems"]["A"]["seeds"]["17"];
        assert_eq!(m["execution_failures"], 2);
        assert_eq!(m["human_fpr_at_0_5"]["count"], 1);
        // g1: h 1, mixed .16, model 1 -> 2.16/3; g2 as before 0.085.
        approx(
            m["balanced_involvement_brier"].as_f64().unwrap(),
            (2.16 / 3.0 + 0.085) / 2.0,
        );
        assert_eq!(m["tpr_at_0_5"]["count"], 3);
    }

    #[test]
    fn paired_difference_is_exact_and_seed_averaged() {
        let rows = fixture();
        let a = sys(
            "A",
            vec![
                (17, preds()),
                (
                    29,
                    preds()
                        .into_iter()
                        .map(|(i, p)| (i, if i == "h1" { 0.4 } else { p }))
                        .collect(),
                ),
            ],
        );
        let b = sys("B", vec![(1, preds())]);
        let e = evaluate(
            &rows,
            &[a, b],
            &[("A".into(), "B".into())],
            &Options::default(),
        )
        .unwrap();
        let r = &e["comparisons"][0]["variants"]["recipe"];
        // Only h1 differs in seed 29: loss .16 vs .04, seed-mean diff .06; h1 weight 1/6.
        approx(r["balanced_brier_difference"].as_f64().unwrap(), 0.06 / 6.0);
        let rel = &e["comparisons"][0]["variants"]["released"];
        approx(rel["balanced_brier_difference"].as_f64().unwrap(), 0.0);
        let c = &rel["balanced_brier_difference_95ci"];
        assert_eq!(
            (c["lower"].as_f64(), c["upper"].as_f64()),
            (Some(0.0), Some(0.0))
        );
        // Per-seed and recipe Brier agree with linearity.
        let sa = &e["systems"]["A"];
        let m17 = sa["seeds"]["17"]["balanced_involvement_brier"]
            .as_f64()
            .unwrap();
        let m29 = sa["seeds"]["29"]["balanced_involvement_brier"]
            .as_f64()
            .unwrap();
        approx(
            sa["recipe"]["balanced_involvement_brier"].as_f64().unwrap(),
            (m17 + m29) / 2.0,
        );
    }

    #[test]
    fn constant_shift_gives_degenerate_interval() {
        // Every human gets loss + c in A (same p shifted), positives identical.
        let rows: Vec<Row> = (0..10)
            .flat_map(|g| {
                vec![
                    row(&format!("h{g}"), &format!("g{g}"), "human_only", 0.0),
                    row(&format!("m{g}"), &format!("g{g}"), "model_only", 1.0),
                ]
            })
            .collect();
        let pa: Vec<(String, f64)> = rows
            .iter()
            .map(|r| (r.id.clone(), if r.is_human() { 0.3 } else { 1.0 }))
            .collect();
        let pb: Vec<(String, f64)> = rows
            .iter()
            .map(|r| (r.id.clone(), if r.is_human() { 0.1 } else { 1.0 }))
            .collect();
        let mk = |n: &str, p: &[(String, f64)]| SystemInput {
            name: n.into(),
            seeds: vec![SeedInput {
                seed: 17,
                predictions: p.iter().map(|(i, x)| (i.clone(), pred(*x))).collect(),
                operating_points: None,
            }],
            subsample: None,
            missing: Missing::Worst,
        };
        let e = evaluate(
            &rows,
            &[mk("A", &pa), mk("B", &pb)],
            &[("A".into(), "B".into())],
            &Options::default(),
        )
        .unwrap();
        let r = &e["comparisons"][0]["variants"]["recipe"];
        // Human cell has half of each group's mass: diff = 0.5 * (0.09 - 0.01).
        approx(r["balanced_brier_difference"].as_f64().unwrap(), 0.04);
        let c = &r["balanced_brier_difference_95ci"];
        approx(c["lower"].as_f64().unwrap(), 0.04);
        approx(c["upper"].as_f64().unwrap(), 0.04);
        // Family-weighted equals balanced here (one doc per cell, two cells per group).
        approx(
            r["family_weighted_brier_difference"].as_f64().unwrap(),
            0.04,
        );
    }

    #[test]
    fn ht_subsample_with_full_sample_equals_full_comparison() {
        let rows = fixture();
        let a = sys("A", vec![(17, preds())]);
        let mut b = sys(
            "B",
            vec![(
                1,
                preds()
                    .into_iter()
                    .map(|(i, p)| (i, (p + 0.1).min(1.0)))
                    .collect(),
            )],
        );
        let full = evaluate(
            &rows,
            &[a, b],
            &[("A".into(), "B".into())],
            &Options::default(),
        )
        .unwrap();
        let a = sys("A", vec![(17, preds())]);
        b = sys(
            "B",
            vec![(
                1,
                preds()
                    .into_iter()
                    .map(|(i, p)| (i, (p + 0.1).min(1.0)))
                    .collect(),
            )],
        );
        b.subsample = Some(rows.iter().map(|r| r.id.clone()).collect());
        b.missing = Missing::Best;
        let ht = evaluate(
            &rows,
            &[a, b],
            &[("A".into(), "B".into())],
            &Options::default(),
        )
        .unwrap();
        let x = full["comparisons"][0]["variants"]["recipe"]["balanced_brier_difference"]
            .as_f64()
            .unwrap();
        let y = ht["comparisons"][0]["variants"]["recipe"]["balanced_brier_difference"]
            .as_f64()
            .unwrap();
        approx(x, y);
        assert_eq!(
            ht["comparisons"][0]["variants"]["recipe"]["estimator"],
            "horvitz_thompson_over_subsample"
        );
    }

    #[test]
    fn ht_subsample_is_unbiased_and_missing_comparator_is_best_case() {
        // Two strata (human, model) with two docs each in two groups.
        let rows = vec![
            row("h1", "g1", "human_only", 0.0),
            row("m1", "g1", "model_only", 1.0),
            row("h2", "g2", "human_only", 0.0),
            row("m2", "g2", "model_only", 1.0),
        ];
        let pa = vec![("h1", 0.2), ("m1", 0.9), ("h2", 0.4), ("m2", 0.7)];
        let truth = {
            let a = sys("A", vec![(17, pa.clone())]);
            let b = sys(
                "B",
                vec![(1, vec![("h1", 0.0), ("m1", 1.0), ("h2", 0.0), ("m2", 1.0)])],
            );
            evaluate(
                &rows,
                &[a, b],
                &[("A".into(), "B".into())],
                &Options::default(),
            )
            .unwrap()["comparisons"][0]["variants"]["recipe"]["balanced_brier_difference"]
                .as_f64()
                .unwrap()
        };
        let mut avg = 0.0;
        for (h, m) in [("h1", "m1"), ("h1", "m2"), ("h2", "m1"), ("h2", "m2")] {
            let a = sys("A", vec![(17, pa.clone())]);
            // B predicts perfectly on its sample; one sampled doc is missing (best case = perfect too).
            let mut b = sys("B", vec![(1, vec![(h, 0.0)])]);
            b.subsample = Some([h.to_string(), m.to_string()].into());
            b.missing = Missing::Best;
            let e = evaluate(
                &rows,
                &[a, b],
                &[("A".into(), "B".into())],
                &Options::default(),
            )
            .unwrap();
            avg += e["comparisons"][0]["variants"]["recipe"]["balanced_brier_difference"]
                .as_f64()
                .unwrap()
                / 4.0;
            assert_eq!(e["systems"]["B"]["seeds"]["1"]["execution_failures"], 1);
        }
        approx(avg, truth);
    }

    #[test]
    fn balanced_weights_match_subsample_module() {
        let rows = fixture();
        let idx: Vec<usize> = (0..rows.len()).collect();
        let docs: Vec<ps::EvalDoc> = rows
            .iter()
            .map(|r| ps::EvalDoc {
                group: r.group.clone(),
                cell: r.cell,
                stratum: String::new(),
                value: 0.0,
            })
            .collect();
        assert_eq!(balanced_weights(&rows, &idx), ps::balanced_weights(&docs));
        let sub = [0, 2, 3, 5];
        let docs: Vec<ps::EvalDoc> = sub.iter().map(|&i| docs[i].clone()).collect();
        assert_eq!(balanced_weights(&rows, &sub), ps::balanced_weights(&docs));
    }

    #[test]
    fn generic_bootstrap_matches_protocol_interval() {
        let v: Vec<f64> = (0..37).map(|i| ((i * 7919) % 101) as f64 / 101.0).collect();
        let d = bootstrap_draws(v.len(), |ix| {
            ix.iter().map(|&j| v[j]).sum::<f64>() / ix.len() as f64
        })
        .unwrap();
        assert_eq!(Some((d[24], d[974])), bootstrap_interval(&v));
    }

    #[test]
    fn clopper_pearson_lower_is_symmetric() {
        approx(clopper_pearson_lower(0, 50, 0.95), 0.0);
        let u = clopper_pearson_upper(3, 40, 0.975);
        let l = clopper_pearson_lower(37, 40, 0.975);
        assert!((1.0 - u - l).abs() < 1e-9);
        // n successes: lower bound = alpha^(1/n).
        assert!((clopper_pearson_lower(20, 20, 0.95) - 0.05f64.powf(1.0 / 20.0)).abs() < 1e-9);
    }

    #[test]
    fn attribution_accuracy_macro_f1_and_other_ai_remap() {
        let mut rows = vec![row("h1", "g1", "human_only", 0.0)];
        let labels = ["a", "a", "b", "b", "olmo"];
        for (k, l) in labels.iter().enumerate() {
            let mut r = row(&format!("p{k}"), &format!("g{k}"), "model_only", 1.0);
            r.label = Some(l.to_string());
            r.held_out_style = k >= 3;
            rows.push(r);
        }
        let vocab = ["a", "b", "other_ai"];
        let guesses = ["a", "b", "b", "a", "other_ai"];
        let mut s = sys("A", vec![(17, vec![("h1", 0.1)])]);
        for (k, gss) in guesses.iter().enumerate() {
            let attribution = vocab
                .iter()
                .map(|v| (v.to_string(), if v == gss { 0.8 } else { 0.1 }))
                .collect();
            s.seeds[0].predictions.insert(
                format!("p{k}"),
                Obs::Ok(Prediction {
                    p_human: 0.0,
                    p_model_only: 1.0,
                    p_mixed: 0.0,
                    share: 1.0,
                    attribution,
                    roles: BTreeMap::new(),
                }),
            );
        }
        s.seeds[0].predictions.insert(
            "h1".into(),
            Obs::Ok(Prediction {
                p_human: 0.9,
                p_model_only: 0.1,
                p_mixed: 0.0,
                share: 0.1,
                attribution: vocab.iter().map(|v| (v.to_string(), 1.0 / 3.0)).collect(),
                roles: BTreeMap::new(),
            }),
        );
        let e = evaluate(&rows, &[s], &[], &Options::default()).unwrap();
        let a = &e["systems"]["A"]["seeds"]["17"]["attribution"];
        approx(a["accuracy"].as_f64().unwrap(), 3.0 / 5.0);
        assert_eq!(a["targets_remapped_to_other_ai"], 1);
        // a: P 1/2 R 1/2 F 1/2; b: P 1/2 R 1/2 F 1/2; other_ai: F 1 -> 2/3.
        approx(a["macro_f1"].as_f64().unwrap(), 2.0 / 3.0);
        approx(
            a["per_generator"]["olmo"]["accuracy"].as_f64().unwrap(),
            1.0,
        );
        let h = &e["systems"]["A"]["seeds"]["17"]["attribution_held_out_voices"];
        assert_eq!(h["documents"], 2); // p3 (b->a wrong), p4 (olmo->other_ai right)
        approx(h["accuracy"].as_f64().unwrap(), 0.5);
    }

    #[test]
    fn slices_follow_the_population_field() {
        let mut rows = fixture();
        rows[2] = row_in("m1", "g1", "model_only", 1.0, "frontier");
        rows[2].label = Some("claude-opus-5-5".into());
        rows[2].family = Some(default_family("claude-opus-5-5"));
        rows.push(row_in("r1", "g3", "mixed", 0.5, "robustness"));
        rows.push(row_in(
            "d1",
            "g4",
            "human_only",
            0.0,
            "sbcsae_spoken_conversation",
        ));
        let sl = build_slices(&rows, &Options::default());
        let get = |d: &str, v: &str| {
            sl.iter()
                .find(|s| s.dimension == d && s.value == v)
                .unwrap()
        };
        // Frontier rows are primary: in the domain slice and the pooled frontier slice.
        assert!(rows[2].primary && rows[2].is_frontier());
        assert_eq!(get("frontier_model", "claude-opus-5-5").idx, vec![0, 2, 3]);
        assert_eq!(get("frontier", "all").idx, vec![0, 2, 3]);
        assert_eq!(get("frontier_voicing", "plain").idx, vec![0, 2, 3]);
        assert_eq!(get("domain", "wikipedia").idx, vec![0, 1, 2, 3, 4, 5]);
        // Robustness: its positives plus every primary human; diagnostic: its own rows.
        let r = get("population", "robustness");
        assert_eq!(
            (r.kind, r.idx.clone(), r.non_primary),
            ("positives_with_all_humans", vec![0, 3, 6], true)
        );
        let d = get("population", "sbcsae_spoken_conversation");
        assert_eq!((d.kind, d.idx.clone()), ("population_rows", vec![7]));
        // Non-primary rows enter no primary slice.
        assert!(
            sl.iter()
                .filter(|s| !s.non_primary)
                .all(|s| s.idx.iter().all(|&i| rows[i].primary))
        );
        assert!(
            !sl.iter()
                .any(|s| s.dimension == "operation" && s.value == "continuation")
        );
        assert_eq!(default_family("requested:gpt-6-astra"), "gpt");
        assert_eq!(default_family("qwen36"), "qwen36");
        // The population field is required, and primary positives must be instruct operations.
        let no_pop = r#"{"id":"a","split":"s","origin":"human_only","share":0,"source_group":"g","domain":"d"}"#;
        assert!(parse_rows(no_pop).is_err());
        let bad = r#"{"id":"a","split":"s","origin":"mixed","share":0.5,"source_group":"g","domain":"d","generator_label":"x","operation":"splice","population":"primary"}"#;
        assert!(parse_rows(bad).is_err());
        let bad = r#"{"id":"a","split":"s","origin":"human_only","share":0,"source_group":"g","domain":"d","population":"frontier"}"#;
        assert!(parse_rows(bad).is_err());
        // Supplementary populations accept the generator's own operation labels,
        // which stay outside the primary metric; primary and robustness stay closed.
        let supp = r#"{"id":"a","split":"s","origin":"mixed","share":0.5,"source_group":"g","domain":"d","generator_label":"x","operation":"polish","population":"supplementary_eval_diverse_not_gated"}"#;
        let r = parse_rows(supp).unwrap();
        assert!(!r[0].primary);
        assert_eq!(r[0].operation.as_deref(), Some("polish"));
        let bad = r#"{"id":"a","split":"s","origin":"mixed","share":0.5,"source_group":"g","domain":"d","generator_label":"x","operation":"polish","population":"primary"}"#;
        assert!(parse_rows(bad).is_err());
        let bad = r#"{"id":"a","split":"s","origin":"mixed","share":0.5,"source_group":"g","domain":"d","generator_label":"x","operation":"polish","population":"robustness_continuations"}"#;
        assert!(parse_rows(bad).is_err());
        let bad = r#"{"id":"a","split":"s","origin":"mixed","share":0.5,"source_group":"g","domain":"d","generator_label":"x","operation":"Bad Op","population":"supplementary_x"}"#;
        assert!(parse_rows(bad).is_err());
    }

    #[test]
    fn sealed_splits_need_explicit_consent_and_inputs_validate() {
        assert!(check_split_permission("dev_iter", None).is_ok());
        assert!(check_split_permission("test", None).is_err());
        assert!(check_split_permission("cal", Some("test")).is_err());
        assert!(check_split_permission("dev_select", Some("dev_select")).is_ok());
        assert!(check_split_permission("dev_iter", Some("dev_iter")).is_err());
        let bad = r#"{"id":"a","split":"s","origin":"human_only","share":0.2,"source_group":"g","domain":"d","population":"primary"}"#;
        assert!(parse_rows(bad).is_err());
        let ok = r#"{"id":"a","split":"s","origin":"mixed","share":0.2,"source_group":"g","domain":"d","generator_label":"x","operation":"span","replacement_spans":[[0,5],[9,12]],"style_axes":{"library_axis":"regional_english","inventor":"mistral7b","voice_ladder":"ab"},"text":"never read","population":"primary"}"#;
        let r = parse_rows(ok).unwrap();
        assert_eq!(r[0].axes, vec!["regional_english".to_string()]);
        // Assembler rows: `human` operation on humans, spans as ai_byte_spans.
        let human = r#"{"id":"h","split":"s","origin":"human_only","share":0,"source_group":"g","domain":"d","generator_label":null,"operation":"human","ai_byte_spans":[],"population":"primary"}"#;
        assert_eq!(parse_rows(human).unwrap()[0].operation, None);
        let pos = r#"{"id":"p","split":"s","origin":"mixed","share":1,"source_group":"g","domain":"d","generator_label":"x","operation":"human","ai_byte_spans":[[0,4]],"population":"primary"}"#;
        assert!(parse_rows(pos).is_err());
        let overlap = r#"{"id":"p","split":"s","origin":"mixed","share":0.5,"source_group":"g","domain":"d","generator_label":"x","operation":"span","ai_byte_spans":[[0,4],[2,6]],"population":"primary"}"#;
        assert!(parse_rows(overlap).is_err());
        let p = parse_predictions(
            r#"{"id":"a","p_human":0.5,"p_model_only":0.2,"p_mixed":0.3,"share":0.4}
{"id":"b","status":"failed","failure":"over_2048_tokens"}
{"id":"c","p_human":null,"p_model_only":0.2,"p_mixed":0.3,"share":0.4}"#,
        )
        .unwrap();
        assert!(matches!(p["a"], Obs::Ok(_)));
        assert_eq!(p["b"], Obs::Failed("over_2048_tokens".into()));
        assert!(matches!(p["c"], Obs::Failed(_)));
        assert!(
            parse_predictions(
                r#"{"id":"a","p_human":0.6,"p_model_only":0.2,"p_mixed":0.3,"share":0.4}"#
            )
            .is_err()
        );
        assert!(parse_predictions(r#"{"id":"a","p_human":0.5,"p_model_only":0.2,"p_mixed":0.3,"share":0.4,"attribution":{"x":0.5,"y":0.4}}"#).is_err());
    }

    fn pred_attr(inv: f64, attr: &[(&str, f64)]) -> Obs {
        Obs::Ok(Prediction {
            p_human: 1.0 - inv,
            p_model_only: inv,
            p_mixed: 0.0,
            share: inv,
            attribution: attr.iter().map(|(k, v)| (k.to_string(), *v)).collect(),
            roles: BTreeMap::new(),
        })
    }

    #[test]
    fn frontier_rows_enter_the_primary_metric_and_the_ht_strata() {
        // g1: human + frontier model-only; g2: human + local model-only.
        let mut rows = vec![
            row("h1", "g1", "human_only", 0.0),
            row_in("f1", "g1", "model_only", 1.0, "frontier"),
            row("h2", "g2", "human_only", 0.0),
            row("m2", "g2", "model_only", 1.0),
        ];
        rows[1].label = Some("claude-opus-5-5".into());
        let p = vec![("h1", 0.2), ("f1", 0.6), ("h2", 0.0), ("m2", 1.0)];
        let e = evaluate(
            &rows,
            &[sys("A", vec![(17, p.clone())])],
            &[],
            &Options::default(),
        )
        .unwrap();
        assert_eq!(e["population"]["primary_documents"], 4);
        // Each cell has weight 1/4: (0.04 + 0.16 + 0 + 0) / 4.
        let m = &e["systems"]["A"]["seeds"]["17"];
        approx(m["balanced_involvement_brier"].as_f64().unwrap(), 0.05);
        // Pooled frontier slice: f1 plus both humans; g1 halves, g2 human only.
        let f = &m["slices"]["frontier"]["all"];
        approx(
            f["balanced_involvement_brier"].as_f64().unwrap(),
            ((0.04 + 0.16) / 2.0 + 0.0) / 2.0,
        );
        // A subsample may select the frontier document: its stratum
        // wikipedia|model_only holds f1 and m2, so pi = 1/2 for f1.
        let mut b = sys("B", vec![(1, vec![("h1", 0.2), ("f1", 1.0), ("h2", 0.0)])]);
        b.subsample = Some(["h1", "f1", "h2"].iter().map(|x| x.to_string()).collect());
        b.missing = Missing::Best;
        let e = evaluate(
            &rows,
            &[sys("A", vec![(17, p)]), b],
            &[("A".into(), "B".into())],
            &Options::default(),
        )
        .unwrap();
        // Paired: only f1 differs (0.16 vs 0), w = 1/4, pi = 1/2 -> 0.08.
        let r = &e["comparisons"][0]["variants"]["recipe"];
        approx(
            r["balanced_brier_difference"].as_f64().unwrap(),
            0.16 * 0.25 / 0.5,
        );
        assert_eq!(r["estimator"], "horvitz_thompson_over_subsample");
        // B standalone HT: losses 0.04, 0, 0 with weights 1/4/1 -> 0.01.
        let bm = &e["systems"]["B"]["seeds"]["1"];
        approx(bm["balanced_involvement_brier"].as_f64().unwrap(), 0.01);
        assert_eq!(bm["estimator"], "horvitz_thompson_over_subsample");
    }

    #[test]
    fn subsample_standalone_metrics_are_unbiased_ht_estimates() {
        // Strata: wikipedia|human_only {h1,h2}, wikipedia|model_only {m1,m2},
        // wikipedia|mixed:[25,50) {x1,x2}. One document per stratum is sampled.
        let rows = vec![
            row("h1", "g1", "human_only", 0.0),
            row("m1", "g1", "model_only", 1.0),
            row("x1", "g1", "mixed", 0.3),
            row("h2", "g2", "human_only", 0.0),
            row("m2", "g2", "model_only", 1.0),
            row("x2", "g2", "mixed", 0.4),
        ];
        let p: Vec<(&str, f64)> = vec![
            ("h1", 0.3),
            ("m1", 0.9),
            ("x1", 0.2),
            ("h2", 0.1),
            ("m2", 0.6),
            ("x2", 0.7),
        ];
        let full = evaluate(
            &rows,
            &[sys("P", vec![(1, p.clone())])],
            &[],
            &Options::default(),
        )
        .unwrap();
        let fm = &full["systems"]["P"]["seeds"]["1"];
        let keys = [
            "/balanced_involvement_brier",
            "/family_weighted_involvement_brier",
            "/share/overall/mae_points",
            "/share/mixed/mae_points",
            "/three_way/balanced_brier_sum3",
            "/human_mean_brier_balanced",
        ];
        let mut avg = [0.0; 6];
        let mut n = 0.0;
        for h in ["h1", "h2"] {
            for m in ["m1", "m2"] {
                for x in ["x1", "x2"] {
                    let mut s = sys(
                        "P",
                        vec![(
                            1,
                            p.iter()
                                .copied()
                                .filter(|(i, _)| [h, m, x].contains(i))
                                .collect(),
                        )],
                    );
                    s.subsample = Some([h, m, x].iter().map(|v| v.to_string()).collect());
                    s.missing = Missing::Best;
                    let e = evaluate(&rows, &[s], &[], &Options::default()).unwrap();
                    let em = &e["systems"]["P"]["seeds"]["1"];
                    for (k, key) in keys.iter().enumerate() {
                        avg[k] += em.pointer(key).unwrap().as_f64().unwrap();
                    }
                    n += 1.0;
                }
            }
        }
        for (k, key) in keys.iter().enumerate() {
            approx(avg[k] / n, fm.pointer(key).unwrap().as_f64().unwrap());
        }
        // A subsample that is the whole split reproduces the full values exactly.
        let mut s = sys("P", vec![(1, p.clone())]);
        s.subsample = Some(rows.iter().map(|r| r.id.clone()).collect());
        s.missing = Missing::Best;
        let e = evaluate(&rows, &[s], &[], &Options::default()).unwrap();
        for key in keys {
            approx(
                e["systems"]["P"]["seeds"]["1"]
                    .pointer(key)
                    .unwrap()
                    .as_f64()
                    .unwrap(),
                fm.pointer(key).unwrap().as_f64().unwrap(),
            );
        }
        assert_eq!(
            e["systems"]["P"]["subsample_missing"]["exceeds_2pct"],
            false
        );
    }

    #[test]
    fn ht_share_mae_difference_matches_full_comparison_at_full_sampling() {
        let rows = fixture();
        let b_preds: Vec<(&str, f64)> = preds()
            .into_iter()
            .map(|(i, p)| (i, (p + 0.1).min(1.0)))
            .collect();
        let full = evaluate(
            &rows,
            &[
                sys("A", vec![(17, preds())]),
                sys("B", vec![(1, b_preds.clone())]),
            ],
            &[("A".into(), "B".into())],
            &Options::default(),
        )
        .unwrap();
        let mut b = sys("B", vec![(1, b_preds)]);
        b.subsample = Some(rows.iter().map(|r| r.id.clone()).collect());
        b.missing = Missing::Best;
        let ht = evaluate(
            &rows,
            &[sys("A", vec![(17, preds())]), b],
            &[("A".into(), "B".into())],
            &Options::default(),
        )
        .unwrap();
        for k in [
            "share_mae_points_difference",
            "family_weighted_brier_difference",
        ] {
            let x = full["comparisons"][0]["variants"]["recipe"][k]
                .as_f64()
                .unwrap();
            let y = ht["comparisons"][0]["variants"]["recipe"][k]
                .as_f64()
                .unwrap();
            approx(x, y);
        }
        // Share errors (pp): B's share is A's plus 0.1, clamped at 1 (m1).
        // A-B per doc: h1 20-30, x1 20-30, m1 0-0, h2 0-10, m2 50-40, m3 30-20.
        // Balanced: g1 cells (-10, -10, 0) / 3; g2 h -10, model (10+10)/2 -> 0.
        let want = ((-20.0 / 3.0) + (-10.0 + 10.0) / 2.0) / 2.0;
        approx(
            full["comparisons"][0]["variants"]["recipe"]["share_mae_points_difference"]
                .as_f64()
                .unwrap(),
            want,
        );
    }

    #[test]
    fn test_split_needs_declared_operating_points() {
        let rows: Vec<Row> = fixture()
            .into_iter()
            .map(|mut r| {
                r.split = "test".into();
                r
            })
            .collect();
        let e = evaluate(
            &rows,
            &[sys("A", vec![(17, preds())])],
            &[],
            &Options::default(),
        );
        assert!(
            e.unwrap_err()
                .to_string()
                .contains("CAL-declared operating points")
        );
        let mut s = sys("A", vec![(17, preds())]);
        s.seeds[0].operating_points = Some(BTreeMap::from([("0.01", 0.65)]));
        assert!(evaluate(&rows, &[s], &[], &Options::default()).is_err());
        let mut s = sys("A", vec![(17, preds())]);
        s.seeds[0].operating_points =
            Some(BTreeMap::from([("0.01", 0.65), ("0.001", f64::INFINITY)]));
        let e = evaluate(&rows, &[s], &[], &Options::default()).unwrap();
        assert_eq!(
            e["systems"]["A"]["seeds"]["17"]["operating_points"]["0.01"]["threshold"]["source"],
            "declared"
        );
        // Dev-iter keeps the labelled in-split diagnostic.
        let e = evaluate(
            &fixture(),
            &[sys("A", vec![(17, preds())])],
            &[],
            &Options::default(),
        )
        .unwrap();
        assert_eq!(
            e["systems"]["A"]["seeds"]["17"]["operating_points"]["0.01"]["threshold"]["source"],
            "in_split_diagnostic"
        );
    }

    #[test]
    fn frontier_attribution_is_top1_among_frontier_labels_and_other_ai() {
        // Frontier roster {fa, fb}; the model's vocabulary {fa, fb, loc, other_ai}
        // lacks fc, so fc's target is other_ai.
        let mut rows = vec![row("h1", "g0", "human_only", 0.0)];
        let spec = [
            ("fa", true),
            ("fa", false),
            ("fb", true),
            ("fc", true),
            ("fb", true),
        ];
        for (k, (l, held)) in spec.iter().enumerate() {
            let mut r = row_in(
                &format!("f{k}"),
                &format!("g{}", k + 1),
                "model_only",
                1.0,
                "frontier",
            );
            r.label = Some(l.to_string());
            r.held_out_style = *held;
            rows.push(r);
        }
        let attrs: [&[(&str, f64)]; 5] = [
            // fa 0.5 beats the collapsed non-frontier mass 0.45.
            &[("fa", 0.5), ("fb", 0.05), ("loc", 0.3), ("other_ai", 0.15)],
            &[("fa", 0.9), ("fb", 0.05), ("loc", 0.05), ("other_ai", 0.0)],
            // Non-frontier mass 0.7 collapses into other_ai: wrong for fb.
            &[("fa", 0.1), ("fb", 0.2), ("loc", 0.4), ("other_ai", 0.3)],
            // fc (outside the vocabulary): other_ai 0.6 wins, correct.
            &[("fa", 0.2), ("fb", 0.2), ("loc", 0.3), ("other_ai", 0.3)],
            &[("fa", 0.1), ("fb", 0.6), ("loc", 0.2), ("other_ai", 0.1)],
        ];
        let mut s = sys("A", vec![(17, vec![("h1", 0.1)])]);
        for (k, a) in attrs.iter().enumerate() {
            s.seeds[0]
                .predictions
                .insert(format!("f{k}"), pred_attr(1.0, a));
        }
        s.seeds[0].predictions.insert(
            "h1".into(),
            pred_attr(
                0.1,
                &[
                    ("fa", 0.25),
                    ("fb", 0.25),
                    ("loc", 0.25),
                    ("other_ai", 0.25),
                ],
            ),
        );
        let opts = Options {
            frontier_labels: vec!["fa".into(), "fb".into(), "fc".into()],
            ..Options::default()
        };
        let e = evaluate(&rows, &[s], &[], &opts).unwrap();
        let m = &e["systems"]["A"]["seeds"]["17"];
        let fa = &m["frontier_attribution"];
        assert_eq!(fa["rule"], "top1_among_frontier_labels_and_other_ai");
        // Held-out voices: f0 right, f2 wrong, f3 right, f4 right -> 3/4.
        approx(fa["held_out_voices"]["accuracy"].as_f64().unwrap(), 0.75);
        assert_eq!(fa["held_out_voices"]["documents"], 4);
        approx(
            fa["held_out_voices"]["per_frontier_label"]["fb"]["accuracy"]
                .as_f64()
                .unwrap(),
            0.5,
        );
        assert_eq!(
            fa["held_out_voices"]["per_frontier_label"]["fc"]["target"],
            "other_ai"
        );
        // All voices add f1 (right): 4/5.
        approx(fa["all_voices"]["accuracy"].as_f64().unwrap(), 0.8);
        // The unrestricted argmax is a separate figure: f0, f1, f4 right; f2 (loc)
        // and f3 (other_ai and loc tie at 0.3, loc is smaller) wrong -> 3/5.
        approx(m["attribution"]["accuracy"].as_f64().unwrap(), 0.6);
        // Per-model slice carries the restricted figure on held-out voices.
        let sl = &m["slices"]["frontier_model"]["fa"]["frontier_attribution"];
        approx(sl["held_out_voices"]["accuracy"].as_f64().unwrap(), 1.0);
        assert_eq!(sl["held_out_voices"]["documents"], 1);
        assert_eq!(e["population"]["frontier_labels_source"], "declared");
        // A frontier row outside a declared roster is refused.
        let opts = Options {
            frontier_labels: vec!["fa".into()],
            ..Options::default()
        };
        let s = sys("A", vec![(17, vec![("h1", 0.1)])]);
        assert!(evaluate(&rows, &[s], &[], &opts).is_err());
    }

    #[test]
    fn gated_frontier_slice_uses_its_realised_minimum() {
        // 160 groups, each one human and one frontier positive, in wikipedia.
        let build = |frontier: usize| -> Vec<Row> {
            let mut v = vec![];
            for g in 0..160 {
                v.push(row(&format!("h{g}"), &format!("g{g}"), "human_only", 0.0));
                if g < frontier {
                    let mut r = row_in(
                        &format!("f{g}"),
                        &format!("g{g}"),
                        "model_only",
                        1.0,
                        "frontier",
                    );
                    r.label = Some("claude-opus-5-5".into());
                    v.push(r);
                }
            }
            v
        };
        let find = |g: &[Value], k: &str| g.iter().find(|x| x["slice"] == k).unwrap().clone();
        let rows = build(150);
        let sl = build_slices(&rows, &Options::default());
        let g = gated_slices(&rows, &sl);
        assert_eq!(g.len(), GATED_DOMAINS.len() + 2);
        let f = find(&g, "frontier:all");
        assert_eq!(f["status"], "gated");
        assert_eq!(f["realised"]["frontier_positives"], 150);
        assert_eq!(find(&g, "domain:wikipedia")["status"], "gated");
        // A listed domain with no documents, and the held-out generator slice
        // without its generator, are reported ungated.
        assert_eq!(find(&g, "domain:reddit")["status"], "reported_ungated");
        assert_eq!(
            find(&g, "held_out_generator:all")["status"],
            "reported_ungated"
        );
        let rows = build(149);
        let sl = build_slices(&rows, &Options::default());
        let f = find(&gated_slices(&rows, &sl), "frontier:all");
        assert_eq!(f["status"], "reported_ungated");
        assert_eq!(
            f["realised"]["shortfall"],
            json!(["frontier_positives 149 < 150"])
        );
        // The per-slice record carries the realised flag.
        let p: Vec<(String, f64)> = rows.iter().map(|r| (r.id.clone(), r.y)).collect();
        let s = SystemInput {
            name: "A".into(),
            seeds: vec![SeedInput {
                seed: 17,
                predictions: p.iter().map(|(i, x)| (i.clone(), pred(*x))).collect(),
                operating_points: None,
            }],
            subsample: None,
            missing: Missing::Worst,
        };
        let e = evaluate(&rows, &[s], &[], &Options::default()).unwrap();
        assert_eq!(
            e["systems"]["A"]["seeds"]["17"]["slices"]["frontier"]["all"]["below_slice_minimum"],
            true
        );
        assert_eq!(
            e["systems"]["A"]["seeds"]["17"]["slices"]["domain"]["wikipedia"]["below_slice_minimum"],
            false
        );
    }

    #[test]
    fn diagnostic_populations_stay_outside_primary_metrics() {
        let mut rows = fixture();
        rows.push(row_in(
            "d1",
            "g9",
            "human_only",
            0.0,
            "ellipse_ell_learner_essays",
        ));
        rows.push(row_in(
            "d2",
            "g9",
            "human_only",
            0.0,
            "ellipse_ell_learner_essays",
        ));
        rows[6].demographics = vec![("ell_status".into(), "ell".into())];
        rows[7].demographics = vec![("ell_status".into(), "ell".into())];
        let mut p = preds();
        p.push(("d1", 0.9));
        p.push(("d2", 0.3));
        let mut s = sys("A", vec![(17, p)]);
        s.seeds[0].operating_points = Some(BTreeMap::from([("0.01", 0.65), ("0.001", 0.95)]));
        let e = evaluate(&rows, &[s], &[], &Options::default()).unwrap();
        let m = &e["systems"]["A"]["seeds"]["17"];
        // Primary metric unchanged by the diagnostic rows.
        let bal = (0.2 / 3.0 + (0.0 + 0.17) / 2.0) / 2.0;
        approx(m["balanced_involvement_brier"].as_f64().unwrap(), bal);
        assert_eq!(m["human_fpr_at_0_5"]["n"], 2);
        // The diagnostic: human FPR at the operating points and mean involvement.
        let d = &m["slices"]["population"]["ellipse_ell_learner_essays"];
        assert_eq!(d["kind"], "population_rows");
        assert_eq!(d["operating_points"]["0.01"]["human_fpr"]["count"], 1);
        assert_eq!(d["operating_points"]["0.001"]["human_fpr"]["count"], 0);
        approx(
            d["human_mean_involvement_unweighted"].as_f64().unwrap(),
            0.6,
        );
        assert_eq!(
            m["slices"]["demographic:ell_status"]["ell"]["origin_counts"]["human_only"],
            2
        );
        assert!(m["slices"]["human_genre"]["unspecified"]["documents"] == 2);
    }

    #[test]
    fn role_attribution_scores_documented_roles_with_fallbacks() {
        // h1 human (enrolled writer w1), x1 mixed (writer unknown, gen-a),
        // m1 model-only (gen-a), m2 model-only (gen-z outside the AI roster).
        let mut rows = vec![
            row("h1", "g1", "human_only", 0.0),
            row("x1", "g2", "mixed", 0.4),
            row("m1", "g3", "model_only", 1.0),
            row("m2", "g4", "model_only", 1.0),
        ];
        rows[0].human_label = Some("w1".into());
        rows[3].label = Some("gen-z".into());
        let role = |profile: &str, inv: f64, named: &[(&str, f64)]| RoleOutput {
            profile: profile.into(),
            involvement_probability: Some(inv),
            named: named.iter().map(|(k, v)| (k.to_string(), *v)).collect(),
        };
        let mk = |inv: f64, h: RoleOutput, a: RoleOutput| {
            Obs::Ok(Prediction {
                p_human: 1.0 - inv,
                p_model_only: inv,
                p_mixed: 0.0,
                share: inv,
                attribution: a.named.clone(),
                roles: BTreeMap::from([("human".to_string(), h), ("ai".to_string(), a)]),
            })
        };
        let hn = [("w1", 0.7), ("w2", 0.3)];
        let an = [("gen-a", 0.8), ("gen-b", 0.2)];
        let mut s = sys("A", vec![(17, vec![])]);
        let p = &mut s.seeds[0].predictions;
        // h1: human role w1 (right); its AI result is not scored (no AI contributor).
        p.insert(
            "h1".into(),
            mk(0.2, role("w1", 0.8, &hn), role("gen-b", 0.2, &an)),
        );
        // x1: human other_human (right: writer not enrolled); AI gen-a (right).
        p.insert(
            "x1".into(),
            mk(0.6, role("other_human", 0.5, &hn), role("gen-a", 0.6, &an)),
        );
        // m1: AI other_ai (wrong: gen-a is in the roster); human role not scored.
        p.insert(
            "m1".into(),
            mk(1.0, role("w2", 0.1, &hn), role("other_ai", 1.0, &an)),
        );
        // m2: AI other_ai (right: gen-z is outside the roster).
        p.insert(
            "m2".into(),
            mk(
                1.0,
                role("other_human", 0.0, &hn),
                role("other_ai", 1.0, &an),
            ),
        );
        let e = evaluate(&rows, &[s], &[], &Options::default()).unwrap();
        let r = &e["systems"]["A"]["seeds"]["17"]["role_attribution"];
        let h = &r["human"];
        assert_eq!(h["documented_documents"], 2);
        assert_eq!(h["undocumented_documents_not_scored"], 2);
        approx(h["accuracy"].as_f64().unwrap(), 1.0);
        assert_eq!(h["fallback"], "other_human");
        // Human role involvement Brier, equal weights 1/4: (0.2^2 + 0.5^2 + 0.1^2 + 0) / 4.
        approx(
            h["role_involvement_balanced_brier"].as_f64().unwrap(),
            (0.04 + 0.25 + 0.01) / 4.0,
        );
        let a = &r["ai"];
        assert_eq!(a["documented_documents"], 3);
        assert_eq!(a["prediction"], "returned_profile");
        assert_eq!(a["targets_remapped_to_fallback"], 1);
        approx(a["accuracy"].as_f64().unwrap(), 2.0 / 3.0);
        // AI involvement Brier: (0.04 + 0.16 + 0 + 0) / 4.
        approx(a["role_involvement_balanced_brier"].as_f64().unwrap(), 0.05);
        // The shared role form parses, and a profile outside its roster is refused.
        let line = r#"{"id":"a","p_human":0.5,"p_model_only":0.2,"p_mixed":0.3,"share":0.4,"attribution":{"human":{"profile":"other_human","involvement_probability":0.8,"conditional_named_scores":{}},"ai":{"profile":"gen-a","involvement_probability":0.5,"conditional_named_scores":{"gen-a":0.9,"gen-b":0.1}}}}"#;
        let parsed = parse_predictions(line).unwrap();
        let Obs::Ok(p) = &parsed["a"] else { panic!() };
        assert_eq!(p.roles["human"].profile, "other_human");
        assert_eq!(p.attribution["gen-a"], 0.9);
        assert!(
            parse_predictions(&line.replace(r#""profile":"gen-a""#, r#""profile":"gen-q""#))
                .is_err()
        );
        // Without role outputs the human role is unavailable and the AI role
        // falls back to the conditional argmax.
        let e = evaluate(
            &fixture(),
            &[sys("A", vec![(17, preds())])],
            &[],
            &Options::default(),
        )
        .unwrap();
        assert_eq!(
            e["systems"]["A"]["seeds"]["17"]["role_attribution"]["human"]["available"],
            false
        );
    }

    #[test]
    fn markdown_renders_every_section() {
        let rows = fixture();
        let e = evaluate(
            &rows,
            &[sys("A", vec![(17, preds())]), sys("B", vec![(1, preds())])],
            &[("A".into(), "B".into())],
            &Options::default(),
        )
        .unwrap();
        let md = markdown(&e);
        for h in [
            "## Primary population",
            "## Recipe",
            "## Paired differences",
            "## Slices for A",
            "### Share by realised-share bin",
        ] {
            assert!(md.contains(h), "missing {h}");
        }
    }
}
