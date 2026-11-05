//! Role schemas for the JSON files, the tokenizer byte-match and the
//! cross-file bindings.
//!
//! Each JSON file must first be in its canonical layout ([`crate::canonical`]),
//! so its length is a function of its content. It is then parsed into a typed
//! schema that refuses unknown fields. Fields that define what the model reads
//! and returns, and the calibration policy, are pinned by the spec; provenance
//! fields are typed (digests, flags, counts, the trainer's arm schema) and
//! free-text provenance strings are free. The scorer's load guard is evaluated
//! with its own predicates ([`crate::guard`]), so an admitted artifact passes
//! it. The bindings then add the export manifest's file digests and the
//! agreement of the arm definition, the heads and the graph class.
//! `heads.json` is also parsed by the scorer's heads runtime.
//!
//! **Digests come from the header (decision recorded 2026-11-05, review
//! finding A5).** The bindings compare the export manifest's file digests
//! with the header's (`files.digests`) instead of a SHA-256 the check
//! computed over each file. No artifact spec version moves for it, because
//! the change is acceptance-neutral on every object a check can meet under
//! admission v2: each file reaches the admission bucket only through a
//! presigned PUT whose signed `x-amz-checksum-sha256` is the header's digest,
//! which R2 verifies at ingest and refuses otherwise (qualification G1),
//! and every reader takes it by entity-tag-pinned ranged reads at the
//! recorded length. For such a file the header digest IS the file's
//! SHA-256, so "manifest digest == header digest" and "manifest digest ==
//! SHA-256(file)" accept and refuse exactly the same artifacts, fixtures
//! and refusal corpus included. A local check before upload (`check`) runs
//! on files the pack wrote and hashed in the same pass; a file changed
//! locally after that is refused by R2 at upload, never admitted. The
//! scorer's `guard_verified` takes the header digest likewise, and
//! `SCORER_GUARD_SHA256` keeps pinning the hashing `guard`, whose body it
//! matches line for line (a test compares them), until the next spec
//! version (planned with decision D5) moves the pin.
//!
//! No byte cap is set here. A file's length is the header's, checked against
//! the bounds of the runtimes that read it ([`crate::spec::RoleSpec`]
//! `max_bytes`) before any byte is read. Under spec v2 and v3 the layout check
//! also prices each file ([`crate::canonical::check_cost`]), and the four
//! costs must fit the spec's value budget (spec v3: the graph class's) before
//! any file is parsed.

// Schema fields are validated by deserialization; many are never read after.
#![allow(dead_code)]

use std::collections::BTreeMap;

use serde::Deserialize;
use serde::de::IgnoredAny;
use serde_json::Value;

use crate::spec::{self, GraphClass, Version};
use crate::{Refusal, sha256_hex};

type Hex64 = String;

/// A field that may be absent, `null` or a value: `None` when absent,
/// `Some(None)` for `null`. Used with `#[serde(default)]` where a schema
/// version decides whether the key exists at all.
fn present<'de, D, T>(d: D) -> Result<Option<Option<T>>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(d).map(Some)
}

fn schema_err(file: &'static str, detail: impl Into<String>) -> Refusal {
    Refusal::Schema {
        file,
        detail: detail.into(),
    }
}

fn is_digest(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

fn digests<'a>(
    file: &'static str,
    items: impl IntoIterator<Item = (&'static str, &'a str)>,
) -> Result<(), Refusal> {
    for (name, d) in items {
        if !is_digest(d) {
            return Err(schema_err(
                file,
                format!("{name} is not a lowercase SHA-256 digest"),
            ));
        }
    }
    Ok(())
}

fn parse<'a, T: Deserialize<'a>>(file: &'static str, bytes: &'a [u8]) -> Result<T, Refusal> {
    serde_json::from_slice(bytes).map_err(|e| schema_err(file, e.to_string()))
}

fn finite_vec(file: &'static str, name: &str, v: &[f64], len: u64) -> Result<(), Refusal> {
    if v.len() as u64 != len || !v.iter().all(|x| x.is_finite()) {
        return Err(schema_err(
            file,
            format!("{name} must be {len} finite numbers"),
        ));
    }
    Ok(())
}

// ------------------------------------------------------------------ shared

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct BundleDigests {
    #[serde(rename = "MANIFEST.json")]
    manifest: Hex64,
    #[serde(rename = "rows.jsonl")]
    rows: Hex64,
    #[serde(rename = "weights.json")]
    weights: Hex64,
}

impl BundleDigests {
    fn check(&self, file: &'static str) -> Result<(), Refusal> {
        digests(
            file,
            [
                ("bundle MANIFEST.json", self.manifest.as_str()),
                ("bundle rows.jsonl", self.rows.as_str()),
                ("bundle weights.json", self.weights.as_str()),
            ],
        )
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileRef {
    file: String,
    sha256: Hex64,
}

// ---------------------------------------------------------------- contract

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct NormalizationRun {
    manifest_sha256: Hex64,
    requests_sha256: Hex64,
    rows_sha256: Hex64,
    run: String,
    shard: u64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct NormalizationSample {
    keep: u64,
    modulus: u64,
    rule: String,
    seed: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct EmbeddingNormalization {
    bundle_sha256: BundleDigests,
    calibration_used_for_fit: bool,
    development_used_for_fit: bool,
    dimension: u64,
    final_test_opened: bool,
    fit: String,
    fit_families: u64,
    fit_rows: u64,
    implementation_sha256: Hex64,
    labels_used_for_fit: bool,
    mean: Vec<f64>,
    plan_sha256: Hex64,
    population: String,
    reference_used_for_fit: bool,
    representation_contract_sha256: Hex64,
    runs: Vec<NormalizationRun>,
    sample: NormalizationSample,
    scale: Vec<f64>,
    scale_floor: f64,
    schema: String,
    v6_artifact_id: Hex64,
    variance: Vec<f64>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ContractV2 {
    ai_profiles: Vec<String>,
    ai_roster_rule: String,
    bundle_manifest_sha256: Hex64,
    embedding_normalization: EmbeddingNormalization,
    embedding_normalization_source_sha256: Hex64,
    final_test_opened: bool,
    human_profiles: Vec<String>,
    input_mode: String,
    maximum_tokens_including_specials: u64,
    origin_order: Vec<String>,
    parent_artifact_id: Hex64,
    raw_dimension: u64,
    schema: String,
    span_coordinates: String,
    span_label_policy: String,
    token_truncation: bool,
    zero_cfg_coordinates: u64,
}

const CONTRACT: &str = "contract.json";

fn check_contract(c: &ContractV2) -> Result<(), Refusal> {
    use spec::contract as p;
    let pinned = c.schema == spec::CONTRACT_ID
        && c.input_mode == p::INPUT_MODE
        && c.raw_dimension == p::RAW_DIMENSION
        && c.maximum_tokens_including_specials == p::MAX_TOKENS_INCLUDING_SPECIALS
        && !c.token_truncation
        && c.origin_order == p::ORIGIN_ORDER
        && c.span_coordinates == p::SPAN_COORDINATES
        && c.span_label_policy == p::SPAN_LABEL_POLICY
        && c.zero_cfg_coordinates == p::ZERO_CFG_COORDINATES
        && c.ai_roster_rule == p::AI_ROSTER_RULE;
    if !pinned {
        return Err(schema_err(
            CONTRACT,
            "a pinned output-contract field differs",
        ));
    }
    if c.final_test_opened || c.embedding_normalization.final_test_opened {
        return Err(schema_err(CONTRACT, "final_test_opened must be false"));
    }
    if c.human_profiles.len() != p::HUMAN_PROFILES
        || c.ai_profiles.len() != p::AI_PROFILES
        || sha256_hex(&serde_json::to_vec(&c.ai_profiles).expect("strings serialize"))
            != p::AI_PROFILES_SHA256
    {
        return Err(schema_err(
            CONTRACT,
            "profile rosters differ from the pinned output contract",
        ));
    }
    digests(
        CONTRACT,
        [
            ("bundle_manifest_sha256", c.bundle_manifest_sha256.as_str()),
            (
                "embedding_normalization_source_sha256",
                c.embedding_normalization_source_sha256.as_str(),
            ),
            ("parent_artifact_id", c.parent_artifact_id.as_str()),
        ],
    )?;
    let n = &c.embedding_normalization;
    if n.schema != p::NORMALIZATION_SCHEMA || n.dimension != p::RAW_DIMENSION {
        return Err(schema_err(
            CONTRACT,
            "embedding normalization schema or dimension",
        ));
    }
    n.bundle_sha256.check(CONTRACT)?;
    digests(
        CONTRACT,
        [
            (
                "normalization implementation_sha256",
                n.implementation_sha256.as_str(),
            ),
            ("normalization plan_sha256", n.plan_sha256.as_str()),
            (
                "normalization representation_contract_sha256",
                n.representation_contract_sha256.as_str(),
            ),
            ("normalization v6_artifact_id", n.v6_artifact_id.as_str()),
        ],
    )?;
    for r in &n.runs {
        digests(
            CONTRACT,
            [
                ("run manifest_sha256", r.manifest_sha256.as_str()),
                ("run requests_sha256", r.requests_sha256.as_str()),
                ("run rows_sha256", r.rows_sha256.as_str()),
            ],
        )?;
    }
    finite_vec(CONTRACT, "normalization mean", &n.mean, p::RAW_DIMENSION)?;
    finite_vec(
        CONTRACT,
        "normalization variance",
        &n.variance,
        p::RAW_DIMENSION,
    )?;
    finite_vec(CONTRACT, "normalization scale", &n.scale, p::RAW_DIMENSION)?;
    if !(n.scale_floor.is_finite() && n.scale_floor > 0.)
        || !n.scale.iter().all(|s| *s >= n.scale_floor)
        || !n.variance.iter().all(|v| *v >= 0.)
    {
        return Err(schema_err(
            CONTRACT,
            "normalization scale below its floor or negative variance",
        ));
    }
    if n.sample.modulus == 0 || n.sample.keep > n.sample.modulus {
        return Err(schema_err(CONTRACT, "normalization sample rule"));
    }
    Ok(())
}

// ------------------------------------------------------------- calibration

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Knot {
    involvement: f64,
    rows: u64,
    score: f64,
    weight: f64,
    weighted_positives: f64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ClippedIsotonicMap {
    algorithm: String,
    bound: String,
    hi: f64,
    input: String,
    interpolation: String,
    knots: Vec<Knot>,
    lo: f64,
    method: String,
    target: String,
    temperature: f64,
    temperature_rule: String,
}

/// The pinned calibration map schema; the heads runtime checks its meaning
/// strings, bounds and monotone knots.
fn check_map(file: &'static str, v: &Value) -> Result<u64, Refusal> {
    let m = ClippedIsotonicMap::deserialize(v)
        .map_err(|e| schema_err(file, format!("calibration map: {e}")))?;
    use spec::calibration as p;
    if m.method != p::METHOD || m.algorithm != p::ALGORITHM || m.interpolation != p::INTERPOLATION {
        return Err(schema_err(
            file,
            "calibration map method differs from the pinned map",
        ));
    }
    if m.knots.is_empty() || m.knots.iter().any(|k| k.rows == 0) {
        return Err(schema_err(file, "calibration knots"));
    }
    crate::involvement_calibration::Map::from_description(v)
        .map_err(|e| schema_err(file, format!("calibration map: {e}")))?;
    Ok(m.knots.len() as u64)
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Clip {
    hi: f64,
    lo: f64,
    rule: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CalibrationPolicy {
    bound: String,
    fallback: String,
    primary: String,
    schema: String,
    selection: String,
    temperature_rule: String,
}

impl CalibrationPolicy {
    fn check(&self) -> Result<(), Refusal> {
        use spec::calibration_policy as p;
        let pinned = self.schema == p::SCHEMA
            && self.primary == p::PRIMARY
            && self.fallback == p::FALLBACK
            && self.selection == p::SELECTION
            && self.bound == p::BOUND
            && self.temperature_rule == p::TEMPERATURE_RULE;
        if pinned {
            Ok(())
        } else {
            Err(schema_err(
                CALIBRATION,
                "calibration policy differs from the pinned policy",
            ))
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Support {
    human_families: u64,
    human_rows: u64,
    positive_families: u64,
    positive_rows: u64,
}

/// The calibration fit's diagnostic record.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
#[allow(non_snake_case)]
struct InvolvementReport {
    clip: Clip,
    isotonic_clipped_in_sample_CAL_brier: f64,
    isotonic_pav_levels: u64,
    limits: Vec<String>,
    origin_temperature: f64,
    origin_temperature_balanced_nll: f64,
    origin_temperature_in_sample_CAL_brier: f64,
    origin_temperature_one_balanced_nll: f64,
    policy: CalibrationPolicy,
    selected: Value,
    selected_method: String,
    selection_reason: String,
    support: Support,
    uncalibrated_CAL_brier: f64,
    weight_formula: String,
    weighting: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Involvement {
    report: InvolvementReport,
    selected: Value,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RoleCalibration {
    qualified: bool,
    roster_empty: bool,
    threshold: f64,
    // Empty roster.
    rule: Option<String>,
    // Qualified roster.
    candidate_count: Option<u64>,
    identity: Option<String>,
    known_correct_named_rate: Option<f64>,
    known_families: Option<u64>,
    known_rows: Option<u64>,
    objective: Option<String>,
    objective_value: Option<f64>,
    presence_gate: Option<String>,
    tie_break: Option<String>,
    weighting: Option<String>,
    withheld_families: Option<u64>,
    withheld_rejection_rate: Option<f64>,
    withheld_rows: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RolesCalibration {
    ai: RoleCalibration,
    human: RoleCalibration,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CalibrationV2 {
    arm: String,
    bundle_sha256: BundleDigests,
    cal_rows: u64,
    complete: bool,
    contract_sha256: Hex64,
    final_test_opened: bool,
    fit_sha256: Hex64,
    fit_split: String,
    implementation_sha256: Hex64,
    involvement: Involvement,
    library_sha256: Hex64,
    limits: Vec<String>,
    policy_sha256: Hex64,
    raw_manifest_sha256: Hex64,
    roles: RolesCalibration,
    schema: String,
    seed: u64,
    selected_checkpoint_sha256: Hex64,
    selection_sha256: Hex64,
    selection_split: String,
    // Calibration v3 (spec v2) only: the exported span emission spec (null
    // without a token head; the heads runtime parses it and the load guard
    // binds it to the heads) and its provenance.
    #[serde(default, deserialize_with = "present")]
    spans: Option<Option<Value>>,
    #[serde(default)]
    spans_provenance: Option<SpansProvenance>,
    #[serde(default)]
    document_calibration_source: Option<DocumentCalibrationSource>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SpansProvenance {
    cal_fit: String,
    cal_fit_sha256: Hex64,
    decoder_chosen_on: String,
    decoder_grid: String,
    decoder_grid_sha256: Hex64,
    maps_fitted_on: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DocumentCalibrationSource {
    file: String,
    note: String,
    sha256: Hex64,
}

const CALIBRATION: &str = "calibration.json";

fn check_role_calibration(role: &str, r: &RoleCalibration) -> Result<(), Refusal> {
    if !(0.0..=1.0).contains(&r.threshold) {
        return Err(schema_err(CALIBRATION, format!("{role} threshold")));
    }
    let qualified_fields = [
        r.candidate_count.is_some(),
        r.identity.is_some(),
        r.known_correct_named_rate.is_some(),
        r.known_families.is_some(),
        r.known_rows.is_some(),
        r.objective.is_some(),
        r.objective_value.is_some(),
        r.presence_gate.is_some(),
        r.tie_break.is_some(),
        r.weighting.is_some(),
        r.withheld_families.is_some(),
        r.withheld_rejection_rate.is_some(),
        r.withheld_rows.is_some(),
    ];
    let ok = if r.roster_empty {
        !r.qualified && r.rule.is_some() && qualified_fields.iter().all(|f| !f)
    } else {
        r.rule.is_none() && qualified_fields.iter().all(|f| *f)
    };
    if !ok {
        return Err(schema_err(
            CALIBRATION,
            format!("{role} role calibration fields"),
        ));
    }
    Ok(())
}

fn check_calibration(c: &CalibrationV2) -> Result<(), Refusal> {
    check_calibration_for(c, spec::calibration::SCHEMA)
}

/// The calibration checks with the schema the export pairs with: calibration
/// v2 has none of the v3 fields; calibration v3 (spec v2) has `spans`.
fn check_calibration_for(c: &CalibrationV2, schema: &str) -> Result<(), Refusal> {
    if c.schema != schema || !c.complete || c.fit_split != spec::calibration::FIT_SPLIT {
        return Err(schema_err(CALIBRATION, "schema, completeness or fit split"));
    }
    let v3_fields = c.spans.is_some()
        || c.spans_provenance.is_some()
        || c.document_calibration_source.is_some();
    if schema == spec::calibration::SCHEMA && v3_fields {
        return Err(schema_err(CALIBRATION, "calibration v2 has no span fields"));
    }
    if schema == spec::CALIBRATION_SCHEMA_V3 && c.spans.is_none() {
        return Err(schema_err(
            CALIBRATION,
            "calibration v3 records its span emission spec",
        ));
    }
    if let Some(p) = &c.spans_provenance {
        digests(
            CALIBRATION,
            [
                ("cal_fit_sha256", p.cal_fit_sha256.as_str()),
                ("decoder_grid_sha256", p.decoder_grid_sha256.as_str()),
            ],
        )?;
    }
    if let Some(d) = &c.document_calibration_source {
        digests(
            CALIBRATION,
            [("document_calibration_source sha256", d.sha256.as_str())],
        )?;
    }
    if c.final_test_opened {
        return Err(schema_err(CALIBRATION, "final_test_opened must be false"));
    }
    if c.cal_rows == 0 {
        return Err(schema_err(CALIBRATION, "cal_rows"));
    }
    c.involvement.report.policy.check()?;
    c.bundle_sha256.check(CALIBRATION)?;
    digests(
        CALIBRATION,
        [
            ("contract_sha256", c.contract_sha256.as_str()),
            ("fit_sha256", c.fit_sha256.as_str()),
            ("implementation_sha256", c.implementation_sha256.as_str()),
            ("library_sha256", c.library_sha256.as_str()),
            ("policy_sha256", c.policy_sha256.as_str()),
            ("raw_manifest_sha256", c.raw_manifest_sha256.as_str()),
            (
                "selected_checkpoint_sha256",
                c.selected_checkpoint_sha256.as_str(),
            ),
            ("selection_sha256", c.selection_sha256.as_str()),
        ],
    )?;
    check_role_calibration("ai", &c.roles.ai)?;
    check_role_calibration("human", &c.roles.human)?;
    let knots = check_map(CALIBRATION, &c.involvement.selected)?;
    // A knot is one distinct CAL score, so there are at most cal_rows.
    if knots > c.cal_rows {
        return Err(schema_err(
            CALIBRATION,
            "more calibration knots than CAL rows",
        ));
    }
    let r = &c.involvement.report;
    if r.selected != c.involvement.selected || r.selected_method != spec::calibration::METHOD {
        return Err(schema_err(
            CALIBRATION,
            "report does not record the selected map",
        ));
    }
    if r.support.human_rows + r.support.positive_rows != c.cal_rows
        || r.support.human_rows == 0
        || r.support.positive_rows == 0
    {
        return Err(schema_err(
            CALIBRATION,
            "report support does not sum to cal_rows",
        ));
    }
    // The clipped map's bounds follow from the CAL support.
    let lo = 1.0 / (2.0 * r.support.human_rows as f64);
    let hi = 1.0 - 1.0 / (2.0 * r.support.positive_rows as f64);
    let sel = &c.involvement.selected;
    if sel["lo"].as_f64() != Some(lo)
        || sel["hi"].as_f64() != Some(hi)
        || r.clip.lo != lo
        || r.clip.hi != hi
    {
        return Err(schema_err(
            CALIBRATION,
            "clip bounds differ from the CAL support rule",
        ));
    }
    Ok(())
}

// ----------------------------------------------------------------- manifest

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Runtime {
    cuda: Option<String>,
    torch: String,
}

// The trainer's arm schema (`train_full_a.py` `validate_arm`).

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ArmSpanHead {
    pooling: String,
    token_loss_weight: f64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ShareBins {
    hidden: u64,
    loss_weight: f64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ShareSupervision {
    bins: Option<ShareBins>,
    conditional_loss_weight: f64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Batching {
    kind: String,
    per_generator: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Reversal {
    delay_epochs: Option<f64>,
    gamma: Option<f64>,
    kind: String,
    max: f64,
    ramp_epochs: Option<f64>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct StyleAdversary {
    axes: bool,
    hidden: u64,
    loss_weight: f64,
    min_cluster_rows: u64,
    reversal: Reversal,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Contrastive {
    loss_weight: f64,
    projection_dim: u64,
    projection_hidden: u64,
    temperature: f64,
}

/// Every field is required, as in the trainer; `null` disables an option.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ArmDefinition {
    batching: Batching,
    #[serde(deserialize_with = "required")]
    contrastive: Option<Contrastive>,
    share_mode: String,
    share_supervision: ShareSupervision,
    #[serde(deserialize_with = "required")]
    span_head: Option<ArmSpanHead>,
    #[serde(deserialize_with = "required")]
    style_adversary: Option<StyleAdversary>,
    // The trainer's spans-v1 fields (`train_full_a.py` `SPAN_ARM_FIELDS`):
    // all three or none. Spec v1 admits none.
    #[serde(default)]
    freeze: Option<String>,
    #[serde(default, deserialize_with = "present")]
    token_head: Option<Option<ArmTokenHead>>,
    #[serde(default)]
    token_weighting: Option<String>,
    // The trainer's spans-S2 field (`train_full_a.py`): present only with the
    // spans-v1 fields. Spec v3 admits the span tower mechanism only, in an
    // export v4; every other export refuses the key.
    #[serde(default, deserialize_with = "present")]
    s2: Option<Option<ArmS2>>,
}

/// The arm's `s2` field of a span tower fit (`full_a_s2.py` `validate`): any
/// other mechanism has other keys and is refused.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ArmS2 {
    layers: u64,
    learning_rate: f64,
    mechanism: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ArmTokenHead {
    hidden: u64,
    learning_rate: f64,
    loss_weight: f64,
}

/// An `Option` field that must be present (possibly `null`).
fn required<'de, D, T>(d: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(d)
}

fn nonneg(x: f64) -> bool {
    x.is_finite() && x >= 0.
}

fn positive(x: f64) -> bool {
    x.is_finite() && x > 0.
}

impl ArmDefinition {
    /// The trainer's `validate_arm` value rules.
    fn check(&self) -> Result<(), Refusal> {
        let bad = |what: &str| Err(schema_err(MANIFEST, format!("arm definition: {what}")));
        if !matches!(
            self.share_mode.as_str(),
            "head" | "token_mean_projected" | "word_token_mean_projected"
        ) {
            return bad("share mode");
        }
        let sv = &self.share_supervision;
        if !nonneg(sv.conditional_loss_weight)
            || sv.bins.as_ref().is_some_and(|b| !positive(b.loss_weight))
        {
            return bad("share supervision");
        }
        match &self.span_head {
            None if self.share_mode != "head" => return bad("token share needs a span head"),
            Some(s)
                if !matches!(s.pooling.as_str(), "attention" | "max")
                    || !nonneg(s.token_loss_weight) =>
            {
                return bad("span head");
            }
            _ => {}
        }
        // spans-v1 fields: all or none, with the trainer's value rules.
        let spans = [
            self.freeze.is_some(),
            self.token_head.is_some(),
            self.token_weighting.is_some(),
        ];
        if spans.iter().any(|f| *f) {
            if !spans.iter().all(|f| *f) {
                return bad("spans-v1 fields");
            }
            let freeze = self.freeze.as_deref().unwrap_or_default();
            let weighting = self.token_weighting.as_deref().unwrap_or_default();
            if !matches!(freeze, "none" | "encoder_and_document_heads")
                || !matches!(weighting, "row_weight" | "bundle_token_weight")
            {
                return bad("freeze / token weighting");
            }
            let th = self.token_head.as_ref().and_then(|t| t.as_ref());
            if let Some(t) = th
                && (!(1..=spec::TOKEN_HEAD_MAX_HIDDEN).contains(&t.hidden)
                    || !positive(t.loss_weight)
                    || !positive(t.learning_rate))
            {
                return bad("token head");
            }
            if freeze == "encoder_and_document_heads"
                && (th.is_none()
                    || self.span_head.is_some()
                    || self.style_adversary.is_some()
                    || self.contrastive.is_some()
                    || self.share_supervision.bins.is_some())
            {
                return bad("a frozen document model trains the token head only");
            }
        }
        // spans-S2: the span tower trains with a frozen document model and its
        // token head (the trainer's `full_a_s2.validate`).
        match &self.s2 {
            None => {}
            Some(None) => return bad("s2 must be absent or a span tower"),
            Some(Some(t)) => {
                let th = self.token_head.as_ref().and_then(|t| t.as_ref());
                if !spans.iter().all(|f| *f)
                    || t.mechanism != spec::tower::MECHANISM
                    || !(1..=spec::tower::MAX_ARM_LAYERS).contains(&t.layers)
                    || !positive(t.learning_rate)
                    || self.freeze.as_deref() != Some(spec::tower::FREEZE)
                    || th.is_none()
                {
                    return bad("s2 span tower");
                }
            }
        }
        let b = &self.batching;
        let batching_ok = match b.kind.as_str() {
            "random" => b.per_generator.is_none(),
            "voice_mixed" => b.per_generator.is_some_and(|n| n >= 1),
            _ => false,
        };
        if !batching_ok {
            return bad("batching");
        }
        if let Some(a) = &self.style_adversary {
            let r = &a.reversal;
            let reversal_ok = matches!(r.kind.as_str(), "constant" | "linear_ramp" | "dann")
                && (0.0..=10.0).contains(&r.max)
                && r.delay_epochs.is_none_or(nonneg)
                && (r.kind == "constant" || r.ramp_epochs.is_some_and(positive))
                && (r.kind != "dann" || r.gamma.is_some_and(positive));
            if !positive(a.loss_weight) || a.min_cluster_rows < 1 || !reversal_ok {
                return bad("style adversary");
            }
        }
        if let Some(c) = &self.contrastive
            && (!positive(c.loss_weight)
                || !positive(c.temperature)
                || c.projection_hidden < 1
                || c.projection_dim < 2
                || b.kind != "voice_mixed")
        {
            return bad("contrastive");
        }
        Ok(())
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FitBinding {
    architecture_sha256: Hex64,
    arm: String,
    arm_definition: ArmDefinition,
    artifact_loader_sha256: Hex64,
    aux_sha256: Hex64,
    bundle_reader_sha256: Hex64,
    bundle_sha256: BundleDigests,
    contract_sha256: Hex64,
    cuda: Option<String>,
    dev_iter_rows_sha256: Hex64,
    device: String,
    initializer_sha256: Hex64,
    model_sha256: Hex64,
    parent_artifact_id: Hex64,
    protocol_sha256: Hex64,
    schema: String,
    seed: u64,
    shared_heads_implementation_sha256: Hex64,
    shared_loss_sha256: Hex64,
    torch: String,
    trainer_sha256: Hex64,
    transformers_model_source_sha256: Hex64,
    // Export v3 bindings (spec v2) only.
    #[serde(default, deserialize_with = "present")]
    init_fit: Option<Option<InitFit>>,
    #[serde(default)]
    spans_rule_sha256: Option<Hex64>,
    // Export v4 bindings (spec v3) only: the spans-S2 module and rule. A key
    // that is present, even as null, counts as present.
    #[serde(default, deserialize_with = "present")]
    s2: Option<Option<FitS2>>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FitS2 {
    mechanism: String,
    module_sha256: Hex64,
    rule: String,
}

/// The fit a frozen-document token head was trained from
/// (`train_full_a.py` `init_record`).
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct InitFit {
    aux_heads: String,
    dev_iter_raw_sha256: Hex64,
    fit_dir: String,
    fit_sha256: Hex64,
    init_arm: String,
    init_seed: u64,
    init_selected_evaluation: u64,
    schema: String,
    selected_checkpoint_sha256: Hex64,
    token_head_restored: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileRefRole {
    file: String,
    role: String,
    sha256: Hex64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct LocalRequests {
    contains_private_text: bool,
    file: String,
    rows: u64,
    rule: String,
    sha256: Hex64,
    split: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ExportManifestV2 {
    arm: String,
    calibrated: bool,
    calibration: FileRef,
    complete: bool,
    contract: FileRef,
    encoder: FileRef,
    exporter_sha256: Hex64,
    final_test_opened: bool,
    fit_binding: FitBinding,
    heads: FileRef,
    limits: Vec<String>,
    parent_artifact_id: Hex64,
    parent_manifest_sha256: Hex64,
    production_qualified: bool,
    reference_device: String,
    reference_predictions: FileRef,
    reference_requests: FileRef,
    runtime: Runtime,
    schema: String,
    seed: u64,
    selected_checkpoint_sha256: Hex64,
    selected_evaluation: u64,
    // Export v3 (spec v2) only; every key is present in an export v3. The
    // diagnostic subset must be null, so its value is skipped, never built.
    #[serde(default, deserialize_with = "present")]
    diagnostic_train_subset: Option<Option<serde::de::IgnoredAny>>,
    #[serde(default, deserialize_with = "present")]
    local_reference_predictions: Option<Option<FileRef>>,
    #[serde(default, deserialize_with = "present")]
    local_reference_predictions_fp32: Option<Option<FileRefRole>>,
    #[serde(default, deserialize_with = "present")]
    local_reference_requests: Option<Option<LocalRequests>>,
    #[serde(default)]
    reference_function: Option<String>,
    #[serde(default)]
    reference_precision: Option<String>,
    #[serde(default)]
    reference_predictions_fp32: Option<FileRefRole>,
    #[serde(default)]
    reference_rotary: Option<String>,
    #[serde(default)]
    reference_rotary_modules: Option<u64>,
    #[serde(default, deserialize_with = "present")]
    span_emission: Option<Option<Value>>,
    #[serde(default, deserialize_with = "present")]
    token_head: Option<Option<Value>>,
    // Export v4 (spec v3) only; every key is present in an export v4. A key
    // that is present, even as null, counts as present.
    #[serde(default, deserialize_with = "present")]
    span_tower: Option<Option<SpanTowerFile>>,
    #[serde(default, deserialize_with = "present")]
    fit_hashes: Option<Option<FitHashes>>,
    #[serde(default, deserialize_with = "present")]
    reference_tower_rotary_modules: Option<Option<u64>>,
}

/// Export v4 `span_tower`: the tower weights file and the declaration.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SpanTowerFile {
    declaration: Value,
    dtype: String,
    file: String,
    parameters: u64,
    sha256: Hex64,
    tensors: u64,
}

/// Export v4 `fit_hashes`: the tower fit's files and the checks that its
/// encoder and document heads are its init fit's, bit for bit.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FitHashes {
    binding_json_sha256: Hex64,
    document_head_tensors: u64,
    document_heads_equal_to_init_fit: bool,
    encoder_equal_to_init_fit: bool,
    encoder_sha256: Hex64,
    fit_json_sha256: Hex64,
    frozen_document_check: FrozenDocumentCheck,
    heads_sha256: Hex64,
    init_fit: FitHashesInit,
    s2_module_sha256: Hex64,
    selected_checkpoint_sha256: Hex64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FrozenDocumentCheck {
    encoder_and_document_heads_bitwise_equal_to_init: bool,
    selected_dev_iter_document_raw_vs_init: DocumentRawCheck,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DocumentRawCheck {
    document_raw_max_abs_error: f64,
    rows: u64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FitHashesInit {
    encoder_sha256: Hex64,
    fit_dir: String,
    fit_sha256: Hex64,
    heads_sha256: Hex64,
    selected_checkpoint_sha256: Hex64,
}

const MANIFEST: &str = "manifest.json";

fn check_manifest(m: &ExportManifestV2) -> Result<(), Refusal> {
    check_manifest_for(m, spec::EXPORT_SCHEMA)
}

/// The export manifest checks with its schema: export v2 has none of the v3
/// fields; export v3 (spec v2) has every one, a null diagnostic subset, and a
/// fit binding with the spans-v1 fields; export v4 (spec v3) also has the
/// span tower fields ([`check_tower_manifest`]), and only it has an arm `s2`.
fn check_manifest_for(m: &ExportManifestV2, schema: &str) -> Result<(), Refusal> {
    if m.schema != schema || !m.complete || !m.calibrated {
        return Err(schema_err(MANIFEST, "incomplete or uncalibrated export"));
    }
    let f = &m.fit_binding;
    let want_v4 = schema == spec::EXPORT_SCHEMA_V4;
    let v4 = [
        m.span_tower.is_some(),
        m.fit_hashes.is_some(),
        m.reference_tower_rotary_modules.is_some(),
        f.s2.is_some(),
        f.arm_definition.s2.is_some(),
    ];
    if !v4.iter().all(|present| *present == want_v4) {
        return Err(schema_err(
            MANIFEST,
            if want_v4 {
                "an export v4 field is missing"
            } else {
                "only an export v4 has span tower fields"
            },
        ));
    }
    let v3 = [
        m.diagnostic_train_subset.is_some(),
        m.local_reference_predictions.is_some(),
        m.local_reference_predictions_fp32.is_some(),
        m.local_reference_requests.is_some(),
        m.reference_function.is_some(),
        m.reference_precision.is_some(),
        m.reference_predictions_fp32.is_some(),
        m.reference_rotary.is_some(),
        m.reference_rotary_modules.is_some(),
        m.span_emission.is_some(),
        m.token_head.is_some(),
        f.init_fit.is_some(),
        f.spans_rule_sha256.is_some(),
    ];
    let want_v3 = schema == spec::EXPORT_SCHEMA_V3 || want_v4;
    if !v3.iter().all(|present| *present == want_v3) {
        return Err(schema_err(
            MANIFEST,
            if want_v3 {
                "an export v3 field is missing"
            } else {
                "export v2 has no export v3 fields"
            },
        ));
    }
    if want_v3 {
        if !matches!(m.diagnostic_train_subset, Some(None)) {
            return Err(schema_err(
                MANIFEST,
                "a diagnostic Train-subset export is never admitted",
            ));
        }
        let mut d = vec![(
            "spans_rule_sha256",
            f.spans_rule_sha256.as_deref().unwrap_or_default(),
        )];
        if let Some(r) = &m.reference_predictions_fp32 {
            d.push(("reference_predictions_fp32 sha256", r.sha256.as_str()));
        }
        if let Some(Some(r)) = &m.local_reference_predictions {
            d.push(("local_reference_predictions sha256", r.sha256.as_str()));
        }
        if let Some(Some(r)) = &m.local_reference_predictions_fp32 {
            d.push(("local_reference_predictions_fp32 sha256", r.sha256.as_str()));
        }
        if let Some(Some(r)) = &m.local_reference_requests {
            d.push(("local_reference_requests sha256", r.sha256.as_str()));
        }
        if let Some(Some(i)) = &f.init_fit {
            d.push((
                "init_fit dev_iter_raw_sha256",
                i.dev_iter_raw_sha256.as_str(),
            ));
            d.push(("init_fit fit_sha256", i.fit_sha256.as_str()));
            d.push((
                "init_fit selected_checkpoint_sha256",
                i.selected_checkpoint_sha256.as_str(),
            ));
        }
        digests(MANIFEST, d)?;
    } else if f.arm_definition.freeze.is_some() {
        return Err(schema_err(
            MANIFEST,
            "arm definition: spans-v1 fields need an export v3",
        ));
    }
    if m.final_test_opened {
        return Err(schema_err(MANIFEST, "final_test_opened must be false"));
    }
    if f.schema != spec::FIT_BINDING_SCHEMA || f.arm != m.arm || f.seed != m.seed {
        return Err(schema_err(MANIFEST, "fit binding schema, arm or seed"));
    }
    f.arm_definition.check()?;
    f.bundle_sha256.check(MANIFEST)?;
    digests(
        MANIFEST,
        [
            ("calibration sha256", m.calibration.sha256.as_str()),
            ("contract sha256", m.contract.sha256.as_str()),
            ("encoder sha256", m.encoder.sha256.as_str()),
            ("heads sha256", m.heads.sha256.as_str()),
            (
                "reference_predictions sha256",
                m.reference_predictions.sha256.as_str(),
            ),
            (
                "reference_requests sha256",
                m.reference_requests.sha256.as_str(),
            ),
            ("exporter_sha256", m.exporter_sha256.as_str()),
            ("parent_artifact_id", m.parent_artifact_id.as_str()),
            ("parent_manifest_sha256", m.parent_manifest_sha256.as_str()),
            (
                "selected_checkpoint_sha256",
                m.selected_checkpoint_sha256.as_str(),
            ),
            ("architecture_sha256", f.architecture_sha256.as_str()),
            ("artifact_loader_sha256", f.artifact_loader_sha256.as_str()),
            ("aux_sha256", f.aux_sha256.as_str()),
            ("bundle_reader_sha256", f.bundle_reader_sha256.as_str()),
            ("fit contract_sha256", f.contract_sha256.as_str()),
            ("dev_iter_rows_sha256", f.dev_iter_rows_sha256.as_str()),
            ("initializer_sha256", f.initializer_sha256.as_str()),
            ("model_sha256", f.model_sha256.as_str()),
            ("fit parent_artifact_id", f.parent_artifact_id.as_str()),
            ("protocol_sha256", f.protocol_sha256.as_str()),
            (
                "shared_heads_implementation_sha256",
                f.shared_heads_implementation_sha256.as_str(),
            ),
            ("shared_loss_sha256", f.shared_loss_sha256.as_str()),
            ("trainer_sha256", f.trainer_sha256.as_str()),
            (
                "transformers_model_source_sha256",
                f.transformers_model_source_sha256.as_str(),
            ),
        ],
    )?;
    let names = [
        (&m.heads, "heads.json"),
        (&m.contract, "contract.json"),
        (&m.calibration, "calibration.json"),
    ];
    if names.iter().any(|(r, n)| r.file != *n) {
        return Err(schema_err(
            MANIFEST,
            "heads, contract and calibration file names",
        ));
    }
    if want_v4 {
        check_tower_manifest(m)?;
    }
    Ok(())
}

/// The span tower fields of an export v4: the fit binding's spans-S2 module
/// and rule, the tower weights file with its declaration's depth, the fit's
/// frozen encoder and document heads, and digests that agree with the rest of
/// the manifest. The declaration itself is bound to the heads' by the load
/// guard and to the graph class's by [`check_roles_with`].
fn check_tower_manifest(m: &ExportManifestV2) -> Result<(), Refusal> {
    let bad = |what: &str| Err(schema_err(MANIFEST, format!("span tower: {what}")));
    let (Some(Some(t)), Some(Some(h)), Some(Some(rotary)), Some(Some(s2)), Some(Some(arm))) = (
        &m.span_tower,
        &m.fit_hashes,
        m.reference_tower_rotary_modules,
        &m.fit_binding.s2,
        &m.fit_binding.arm_definition.s2,
    ) else {
        return bad("fields");
    };
    if s2.mechanism != spec::tower::MECHANISM || s2.rule != spec::tower::RULE {
        return bad("fit binding mechanism or rule");
    }
    let depth = t.declaration["layers"]
        .as_array()
        .map_or(0, |l| l.len() as u64);
    if depth == 0 || t.declaration["weights"] != spec::tower::WEIGHTS {
        return bad("declaration");
    }
    if t.dtype != "F32"
        || t.file != spec::tower::WEIGHTS
        || t.tensors != 6 * depth + 1
        || t.parameters != spec::tower::LAYER_PARAMETERS * depth + spec::tower::HIDDEN
    {
        return bad("weights file");
    }
    if arm.layers != depth || rotary != depth {
        return bad("arm layers or rotary modules differ from the declaration's depth");
    }
    let frozen = &h.frozen_document_check;
    if !h.encoder_equal_to_init_fit
        || !h.document_heads_equal_to_init_fit
        || !frozen.encoder_and_document_heads_bitwise_equal_to_init
        || frozen
            .selected_dev_iter_document_raw_vs_init
            .document_raw_max_abs_error
            != 0.0
        || frozen.selected_dev_iter_document_raw_vs_init.rows == 0
        || h.document_head_tensors == 0
    {
        return bad("the encoder and document heads must be the init fit's");
    }
    digests(
        MANIFEST,
        [
            ("span_tower sha256", t.sha256.as_str()),
            ("fit binding s2 module_sha256", s2.module_sha256.as_str()),
            (
                "fit_hashes binding_json_sha256",
                h.binding_json_sha256.as_str(),
            ),
            ("fit_hashes encoder_sha256", h.encoder_sha256.as_str()),
            ("fit_hashes fit_json_sha256", h.fit_json_sha256.as_str()),
            ("fit_hashes heads_sha256", h.heads_sha256.as_str()),
            ("fit_hashes s2_module_sha256", h.s2_module_sha256.as_str()),
            (
                "fit_hashes selected_checkpoint_sha256",
                h.selected_checkpoint_sha256.as_str(),
            ),
            (
                "fit_hashes init encoder_sha256",
                h.init_fit.encoder_sha256.as_str(),
            ),
            ("fit_hashes init fit_sha256", h.init_fit.fit_sha256.as_str()),
            (
                "fit_hashes init heads_sha256",
                h.init_fit.heads_sha256.as_str(),
            ),
            (
                "fit_hashes init selected_checkpoint_sha256",
                h.init_fit.selected_checkpoint_sha256.as_str(),
            ),
        ],
    )?;
    if h.selected_checkpoint_sha256 != m.selected_checkpoint_sha256
        || h.encoder_sha256 != m.encoder.sha256
        || h.init_fit.encoder_sha256 != h.encoder_sha256
        || h.s2_module_sha256 != s2.module_sha256
        || !matches!(&m.fit_binding.init_fit, Some(Some(i))
            if i.fit_sha256 == h.init_fit.fit_sha256
                && i.selected_checkpoint_sha256 == h.init_fit.selected_checkpoint_sha256)
    {
        return bad("fit_hashes differ from the manifest");
    }
    Ok(())
}

// -------------------------------------------------------------------- heads

const HEADS: &str = "heads.json";

/// The fields of `heads.json` the spec pins; the heads runtime parses the
/// whole file with its own schema.
#[derive(Debug, Deserialize)]
struct HeadsView {
    schema: String,
    architecture: BTreeMap<String, u64>,
    span_head: Option<Value>,
    /// Heads v2 (spec v2): the spans-v1 token head declaration.
    #[serde(default)]
    token_head: Option<TokenHeadView>,
    /// Heads v3 (spec v3): the span tower declaration.
    #[serde(default)]
    span_tower: Option<Value>,
    share_mode: String,
    contract_sha256: String,
    base_temperature: f64,
    profiles: BTreeMap<String, Vec<String>>,
    thresholds: BTreeMap<String, f64>,
    involvement_calibration: Value,
    supervision: BTreeMap<String, u64>,
    weights: BTreeMap<String, HeadsTensor>,
}

/// The top-level members heads v3 may have: the pinned heads runtime's fields
/// (`full_a.rs`, `Artifact`, which refuses any other) and the span tower
/// declaration. Heads v3 are read with it before the span tower runtime, so
/// an unknown or a repeated member is refused in one pass over the members;
/// the runtime's own repeated-member check compares each member with every
/// member before it. Presence and values are checked by [`HeadsView`] and
/// the runtimes.
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct HeadsV3Members {
    schema: IgnoredAny,
    architecture: IgnoredAny,
    span_head: IgnoredAny,
    token_head: IgnoredAny,
    span_emission: IgnoredAny,
    share_mode: IgnoredAny,
    contract_sha256: IgnoredAny,
    initialization_sha256: IgnoredAny,
    base_temperature: IgnoredAny,
    text_mean: IgnoredAny,
    text_scale: IgnoredAny,
    profiles: IgnoredAny,
    thresholds: IgnoredAny,
    involvement_calibration: IgnoredAny,
    supervision: IgnoredAny,
    origin_order: IgnoredAny,
    weights: IgnoredAny,
    span_tower: IgnoredAny,
}

/// Refuses heads v3 with a top-level member that is not a [`HeadsV3Members`]
/// field, or with a repeated one, at that member.
fn check_heads_v3_members(bytes: &[u8]) -> Result<(), Refusal> {
    serde_json::from_slice::<HeadsV3Members>(bytes)
        .map(drop)
        .map_err(|e| schema_err(HEADS, format!("heads v3 members: {e}")))
}

const SUPERVISION_KEYS: [&str; 5] = ["ai", "human", "origin", "share", "token"];

#[derive(Debug, Deserialize)]
struct TokenHeadView {
    hidden: u64,
    input: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct HeadsTensor {
    shape: Vec<u64>,
    values: Vec<f64>,
}

/// The four JSON role files, with the SHA-256 digests of the three that the
/// export manifest binds by digest. The digests come from the artifact
/// header (lowercase hex), which the object store verified against these
/// exact bytes at ingest; the check compares them and never hashes a file.
#[derive(Clone, Copy, Debug)]
pub struct RoleFiles<'a> {
    pub heads: &'a [u8],
    pub contract: &'a [u8],
    pub calibration: &'a [u8],
    pub manifest: &'a [u8],
    pub digests: RoleDigests<'a>,
}

/// The header's SHA-256 (lowercase hex) of `heads.json`, `contract.json`
/// and `calibration.json`.
#[derive(Clone, Copy, Debug)]
pub struct RoleDigests<'a> {
    pub heads: &'a str,
    pub contract: &'a str,
    pub calibration: &'a str,
}

impl<'a> RoleDigests<'a> {
    /// The digests a header lists for the three files, as owned hex, or the
    /// missing file.
    pub fn hex_of(header: &slop_ninja_protocol::ArtifactHeader) -> Result<[String; 3], Refusal> {
        let of = |name: &'static str| {
            header
                .files
                .iter()
                .find(|f| f.name == name)
                .map(|f| hex::encode(f.sha256.0))
                .ok_or(Refusal::MissingFile(name))
        };
        Ok([of(HEADS)?, of(CONTRACT)?, of(CALIBRATION)?])
    }

    /// Borrows the three hex digests of [`RoleDigests::hex_of`].
    pub fn of(hex: &'a [String; 3]) -> Self {
        Self {
            heads: &hex[0],
            contract: &hex[1],
            calibration: &hex[2],
        }
    }
}

/// Content-free facts from the role files.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct RolesReport {
    pub span_head: bool,
    /// Spec v2: whether the heads carry a spans-v1 token head (omitted when
    /// false, so a spec v1 report keeps its fields).
    #[serde(skip_serializing_if = "is_false")]
    pub token_head: bool,
    /// Spec v3: whether the heads declare a span tower (heads v3; omitted when
    /// false, so earlier reports keep their fields).
    #[serde(skip_serializing_if = "is_false")]
    pub span_tower: bool,
    pub heads_parameters: u64,
    pub contract_sha256: String,
    pub arm: String,
    pub seed: u64,
    /// Spec v2 and later: the four JSON role files' summed value cost
    /// ([`crate::canonical::VALUE_COST_ID`]); omitted under spec v1, so a
    /// spec v1 report keeps its fields.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub json_value_bytes: Option<u64>,
}

fn is_false(b: &bool) -> bool {
    !*b
}

/// Refuses a tokenizer that is not byte-identical to the pinned one.
pub fn check_tokenizer(sha256: &str, len: u64) -> Result<(), Refusal> {
    if sha256 != spec::TOKENIZER_SHA256 || len != spec::TOKENIZER_BYTES {
        return Err(Refusal::Tokenizer);
    }
    Ok(())
}

/// Refuses a JSON role file that is not in its canonical layout.
pub fn check_layout(name: &'static str, bytes: &[u8]) -> Result<(), Refusal> {
    check_layout_cost(name, bytes).map(|_| ())
}

/// [`check_layout`], returning the file's value cost
/// ([`crate::canonical::check_cost`]).
pub fn check_layout_cost(name: &'static str, bytes: &[u8]) -> Result<u64, Refusal> {
    let Some(layout) = spec::role_by_name(name).and_then(|r| r.layout) else {
        return Err(Refusal::UndeclaredFile(name.into()));
    };
    crate::canonical::check_cost(bytes, layout)
        .map_err(|detail| Refusal::JsonLayout { file: name, detail })
}

/// Checks the role schemas and the bindings between the role files and, when
/// known, the graph class (spec v1).
pub fn check_roles(
    files: &RoleFiles<'_>,
    class: Option<&GraphClass>,
) -> Result<RolesReport, Refusal> {
    check_roles_for(files, class, Version::V1)
}

/// [`check_roles`] under `version`.
///
/// Spec v1 admits heads v1 in export v2 with calibration v2, without a span
/// head unless the class reads token states. Spec v2 also admits heads v2
/// with a spans-v1 token head in export v3 with calibration v3. Spec v3 also
/// admits heads v3, whose token head reads the span tower's states, in export
/// v4 with calibration v3, on the tower class only; heads v3 declaring another
/// span tower than that class's are refused even when `class` is not given.
/// R0's form (export v2, no span head) is bound by the scorer's release guard
/// (the mirror in [`crate::guard`]); every other form by the scorer's shared
/// load guard (`load_guard.rs`), compiled here.
pub fn check_roles_for(
    files: &RoleFiles<'_>,
    class: Option<&GraphClass>,
    version: Version,
) -> Result<RolesReport, Refusal> {
    check_roles_with(files, class, version, None)
}

/// [`check_roles_for`] with a caller's value budget on top of the version's
/// (the smaller applies; the version's is the class's when `class` is given).
/// The layout checks run first and price each file; a sum past the budget is
/// refused (`json_value_budget`) before any file is parsed.
pub fn check_roles_with(
    files: &RoleFiles<'_>,
    class: Option<&GraphClass>,
    version: Version,
    value_budget: Option<u64>,
) -> Result<RolesReport, Refusal> {
    let costs = [
        check_layout_cost(CONTRACT, files.contract)?,
        check_layout_cost(CALIBRATION, files.calibration)?,
        check_layout_cost(MANIFEST, files.manifest)?,
        check_layout_cost(HEADS, files.heads)?,
    ];
    let value_bytes = costs.iter().fold(0u64, |a, c| a.saturating_add(*c));
    let own = match class {
        Some(c) => version.max_json_value_bytes_for(c),
        None => version.max_json_value_bytes(),
    };
    let budget = match (own, value_budget) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    };
    if let Some(budget) = budget
        && value_bytes > budget
    {
        return Err(Refusal::JsonValueBudget {
            need: value_bytes,
            budget,
        });
    }
    let contract: ContractV2 = parse(CONTRACT, files.contract)?;
    check_contract(&contract)?;
    let heads: HeadsView = parse(HEADS, files.heads)?;
    // The heads schema decides the export and calibration schemas it pairs
    // with: heads v2 from spec v2 on, heads v3 from spec v3 on (v3 and v4), else heads v1.
    let heads_v2 = heads.schema == spec::HEADS_SCHEMA_V2 && version != Version::V1;
    let heads_v3 = heads.schema == spec::HEADS_SCHEMA_V3 && version.has_tower();
    let (export_schema, calibration_schema, supervision_keys, token_input): (_, _, &[&str], _) =
        if heads_v3 {
            (
                spec::EXPORT_SCHEMA_V4,
                spec::CALIBRATION_SCHEMA_V3,
                &spec::SUPERVISION_KEYS_V2,
                spec::TOKEN_HEAD_INPUT_V3,
            )
        } else if heads_v2 {
            (
                spec::EXPORT_SCHEMA_V3,
                spec::CALIBRATION_SCHEMA_V3,
                &spec::SUPERVISION_KEYS_V2,
                spec::TOKEN_HEAD_INPUT_V2,
            )
        } else {
            (
                spec::EXPORT_SCHEMA,
                spec::calibration::SCHEMA,
                &SUPERVISION_KEYS,
                spec::TOKEN_HEAD_INPUT_V2,
            )
        };
    let calibration: CalibrationV2 = parse(CALIBRATION, files.calibration)?;
    check_calibration_for(&calibration, calibration_schema)?;
    let manifest: ExportManifestV2 = parse(MANIFEST, files.manifest)?;
    check_manifest_for(&manifest, export_schema)?;
    // The heads runtime: heads v3 through the span tower runtime, which checks
    // what v3 adds and hands the pinned runtime the same object as heads v2.
    // Heads v3's members are checked first, in one pass, then that they carry
    // a token head and that their span tower is the tower class's, the only
    // class that takes heads v3. The runtime builds the layer list a
    // declaration's `encoder_layers` implies before comparing it, so no other
    // declaration reaches it.
    if heads_v3 {
        check_heads_v3_members(files.heads)?;
        if heads.token_head.is_none() {
            return Err(schema_err(
                HEADS,
                "heads v3 carry the token head the span tower feeds",
            ));
        }
        let Some(tower) = &heads.span_tower else {
            return Err(schema_err(HEADS, "heads v3 declare a span tower"));
        };
        if *tower != spec::tower::declaration() {
            return Err(Refusal::GraphClass(format!(
                "graph class {} holds another span tower than the heads declare",
                spec::TOWER_GRAPH.id
            )));
        }
        crate::full_a_tower::SpanTowerHeads::from_json(files.heads)
            .map_err(|e| schema_err(HEADS, format!("heads runtime: {e:#}")))?;
    } else {
        crate::full_a::FullAHeads::from_json(files.heads)
            .map_err(|e| schema_err(HEADS, format!("heads runtime: {e}")))?;
    }

    // Pinned heads fields.
    use spec::heads as p;
    let arch = |k: &str| heads.architecture.get(k).copied();
    let pinned = (heads.schema == p::SCHEMA || heads_v2 || heads_v3)
        && arch("dimension") == Some(p::DIMENSION)
        && arch("raw_text_dimension") == Some(p::RAW_TEXT_DIMENSION)
        && arch("hidden") == Some(p::HIDDEN)
        && arch("embedding") == Some(p::EMBEDDING)
        && arch("human_profiles") == Some(spec::contract::HUMAN_PROFILES as u64)
        && arch("ai_profiles") == Some(spec::contract::AI_PROFILES as u64);
    if !pinned {
        return Err(schema_err(
            HEADS,
            "schema or architecture differs from the pinned heads",
        ));
    }
    if let Some(t) = &heads.token_head
        && t.input != token_input
    {
        return Err(schema_err(HEADS, "token head input"));
    }
    check_map(HEADS, &heads.involvement_calibration)?;
    if !heads
        .supervision
        .keys()
        .map(String::as_str)
        .eq(supervision_keys.iter().copied())
    {
        return Err(schema_err(HEADS, "supervision fields"));
    }
    let mut heads_parameters = 0u64;
    for (name, t) in &heads.weights {
        let n = t.shape.iter().try_fold(1u64, |a, &d| a.checked_mul(d));
        if n != Some(t.values.len() as u64) || !t.values.iter().all(|v| v.is_finite()) {
            return Err(schema_err(HEADS, format!("weight {name}: shape or values")));
        }
        heads_parameters += t.values.len() as u64;
    }

    // The scorer's load guards, with their own predicates: the release guard
    // for R0's form, the shared load guard for every other form.
    let span_head = heads.span_head.is_some();
    let token_head = heads.token_head.is_some();
    if !heads_v2 && !heads_v3 && !span_head {
        crate::guard::scorer_guard(files)
            .map_err(|e| Refusal::Binding(format!("scorer guard: {e}")))?;
    } else if version != Version::V1 {
        crate::guard::load_guard(files)
            .map_err(|e| Refusal::Binding(format!("scorer load guard: {e}")))?;
    } else {
        // Spec v1 has only the release guard, which refuses a span head.
        crate::guard::scorer_guard(files)
            .map_err(|e| Refusal::Binding(format!("scorer guard: {e}")))?;
    }

    // Bindings the guards do not make: the export manifest's digests (against
    // the header's store-verified digests, so no file is hashed here) and the
    // agreement of the arm definition, the heads and the graph class.
    let contract_sha = files.digests.contract.to_owned();
    let bind = |ok: bool, what: &str| {
        if ok {
            Ok(())
        } else {
            Err(Refusal::Binding(what.into()))
        }
    };
    bind(
        manifest.heads.sha256 == files.digests.heads,
        "export manifest heads digest",
    )?;
    bind(
        manifest.contract.sha256 == contract_sha,
        "export manifest contract digest",
    )?;
    bind(
        manifest.calibration.sha256 == files.digests.calibration,
        "export manifest calibration digest",
    )?;
    bind(
        manifest.fit_binding.contract_sha256 == contract_sha,
        "fit binding contract digest",
    )?;
    let arm = &manifest.fit_binding.arm_definition;
    bind(
        arm.span_head.is_some() == span_head,
        "arm definition and heads disagree on the span head",
    )?;
    bind(
        arm.share_mode == heads.share_mode,
        "arm definition and heads share mode",
    )?;
    let arm_token_head = arm.token_head.as_ref().and_then(|t| t.as_ref());
    bind(
        arm_token_head.is_some() == token_head
            && arm_token_head
                .zip(heads.token_head.as_ref())
                .is_none_or(|(a, h)| a.hidden == h.hidden),
        "arm definition and heads disagree on the token head",
    )?;
    // Spec v1: a span class needs a span head. Spec v2: a class that outputs
    // token states needs heads that read them (a span head or a token head).
    // Spec v3: also, a class that outputs the span tower's states needs heads
    // v3, and only it takes heads v3 (whose span tower is that class's,
    // checked before the span tower runtime).
    let reads_token_states = match version {
        Version::V1 => span_head,
        Version::V2 | Version::V3 | Version::V4 => span_head || (token_head && !heads_v3),
    };
    if let Some(class) = class {
        if class.span_head != reads_token_states {
            return Err(Refusal::GraphClass(match version {
                Version::V1 => format!(
                    "graph class {} {} a span head",
                    class.id,
                    if class.span_head { "needs" } else { "has no" }
                ),
                Version::V2 | Version::V3 | Version::V4 => format!(
                    "graph class {} {} heads that read token states",
                    class.id,
                    if class.span_head { "needs" } else { "takes no" }
                ),
            }));
        }
        if class.tower_states != heads_v3 {
            return Err(Refusal::GraphClass(format!(
                "graph class {} {} heads v3 with its span tower",
                class.id,
                if class.tower_states {
                    "needs"
                } else {
                    "takes no"
                }
            )));
        }
    }
    Ok(RolesReport {
        span_head,
        token_head,
        span_tower: heads_v3,
        heads_parameters,
        contract_sha256: contract_sha,
        arm: manifest.arm,
        seed: manifest.seed,
        json_value_bytes: (version != Version::V1).then_some(value_bytes),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    /// The field names a serde unknown-field error lists after `expected`.
    fn listed(error: &str) -> BTreeSet<String> {
        let list = error
            .split_once("expected one of ")
            .expect("an unknown-field error")
            .1;
        let list = list.split(" at line ").next().unwrap();
        list.split(", ")
            .map(|f| f.trim_matches('`').to_string())
            .collect()
    }

    #[test]
    fn the_heads_v3_members_are_the_pinned_fields_and_the_span_tower() {
        let unknown = br#"{"not_a_heads_member":null}"#;
        let pinned = crate::full_a::FullAHeads::from_json(unknown)
            .err()
            .expect("the pinned runtime refuses an unknown member")
            .to_string();
        let ours = check_heads_v3_members(unknown).unwrap_err().to_string();
        let mut want = listed(&pinned);
        assert!(want.insert("span_tower".into()));
        assert_eq!(listed(&ours), want);
    }

    #[test]
    fn an_unknown_or_repeated_member_is_refused_where_it_is_read() {
        // The rest after the refused member is not JSON: the refusal is the
        // member's, so nothing after it was read.
        let tail = "\"x\" ".repeat(1 << 16);
        for head in [
            r#"{"schema":"a","not_a_heads_member":null,"#.to_string(),
            r#"{"schema":"a","schema":"b","#.to_string(),
        ] {
            let bytes = format!("{head}{tail}");
            let e = check_heads_v3_members(bytes.as_bytes()).unwrap_err();
            assert_eq!(e.code(), "schema");
            let detail = e.to_string();
            assert!(
                detail.starts_with("heads.json: heads v3 members: ")
                    && (detail.contains("unknown field") || detail.contains("duplicate field")),
                "{detail}"
            );
        }
        // The members alone pass; the runtimes check their values.
        let members = r#"{"schema":0,"span_tower":0,"token_head":0,"weights":0}"#;
        assert!(check_heads_v3_members(members.as_bytes()).is_ok());
    }
}
