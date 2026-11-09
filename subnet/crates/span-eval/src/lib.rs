//! The span gate's statistics and decision, rule `slop_ninja_span_gate_v1`,
//! under the evaluation floor `slop_ninja_span_eval_floor24_v1`.
//!
//! Everything here is exact integer arithmetic on one document's exact text,
//! its label and the spans a scorer decoded for it, so the coordinator (Wasm),
//! the local scorer and the evaluator compute the same numbers. Every vector
//! is sized from the document (its spans, its sentences); nothing is tuned.
//!
//! **Populations** (batch documents only; [`Population`]):
//! - *in scope* (`M*`): kind `mixed` whose gold run holds at least
//!   [`FLOOR_WORDS`] words. `sentence_splice_v1` splices one contiguous run of
//!   whole sentences, so an in-scope document has one gold run and no short
//!   run, and the floor rule (short gold runs leave the recall denominator and
//!   their characters count neither way) has nothing to remove;
//! - *human* (`H*`): kind `human_only`;
//! - every other document is reported only.
//!
//! **Characters** are the non-space characters of the exact text (Unicode
//! scalars that are not [`is_space`]), as in the floor-24 evaluator. With `G`
//! the gold run's characters, `P` the union of the decoded spans' characters
//! and `U` the document's characters outside `G`:
//! - `f1_ppb = floor(2 10^9 |P ∩ G| / (|P| + |G|))` (0 when nothing is decoded);
//! - `fp_human = |P ∩ U|`, `human = |U|`;
//! - a human document is `flagged` when any span is decoded, the prediction
//!   failed, or the prediction succeeded and span emission failed.
//!
//! **The gate** ([`verdict`]) compares an entrant with a fixed anchor scored
//! on the same documents: structural conditions first (a token head, the
//! decoder rule with a minimum of at least 24 words, emission failures), then
//! the three measured conditions, each passing when the entrant is no worse
//! than the anchor by more than a frozen margin ([`Margins`]):
//! - Q1 `Σf1_e + Δ_F1 |M*| ≥ Σf1_A`;
//! - QC `10^9 Σfp_e ≤ 10^9 Σfp_A + Δ_C Σhuman`;
//! - QH `H_e ≤ H_A + Δ_H`.
//!
//! The first failing condition, in [`Reason`] order, is the entrant's one
//! reason. [`trivial`] evaluates the content-free policies that bound what a
//! head with no localization signal reaches, for the per-cohort anchor check.

#![forbid(unsafe_code)]

pub mod trivial;

/// The gate rule.
pub const GATE_RULE: &str = "slop_ninja_span_gate_v1";
/// The evaluation floor rule (the floor-24 evaluator's rule).
pub const EVAL_RULE: &str = "slop_ninja_span_eval_floor24_v1";
/// The only decoder rule the gate admits.
pub const DECODER_RULE: &str = "slop_ninja_span_decoder_v1";
/// The words rule: maximal runs of non-[`is_space`] characters.
pub const WORD_RULE: &str = "slop_ninja_share_words_v1";
/// The 24-word floor: no span under it is trained, scored or decoded.
pub const FLOOR_WORDS: u32 = 24;
/// Parts per billion: the scale of every rate here.
pub const PPB: u64 = 1_000_000_000;

/// Python `str.isspace`: Unicode White_Space plus U+001C..U+001F (the
/// runtime's rule, `full_a_spans::is_space`).
pub fn is_space(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}

/// Words of `s` under [`WORD_RULE`].
pub fn words(s: &str) -> u32 {
    let mut n = 0u32;
    let mut inside = false;
    for c in s.chars() {
        let w = !is_space(c);
        if w && !inside {
            n = n.saturating_add(1);
        }
        inside = w;
    }
    n
}

/// One decoded span as the scorer returns it (`slop_ninja_span_emission_v1`):
/// UTF-8 byte offsets into the exact text, half-open, and its word count.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Span {
    pub start_byte: u32,
    pub end_byte: u32,
    pub words: u32,
}

/// A document's label kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Mixed,
    HumanOnly,
    ModelOnly,
    Other,
}

impl Kind {
    /// The kind of a label string (`mixed`, `human_only`, `model_only`).
    pub fn of(label: &str) -> Kind {
        match label {
            "mixed" => Kind::Mixed,
            "human_only" => Kind::HumanOnly,
            "model_only" => Kind::ModelOnly,
            _ => Kind::Other,
        }
    }
}

/// Which gate population a batch document belongs to.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub enum Population {
    /// Reported only: out-of-scope mixed, model-only, and anything else.
    #[default]
    Reported = 0,
    /// `M*`: mixed with a gold run of at least [`FLOOR_WORDS`] words.
    InScope = 1,
    /// `H*`: human-only.
    Human = 2,
}

impl Population {
    pub fn tag(self) -> u8 {
        self as u8
    }

    pub fn from_tag(t: u8) -> Option<Self> {
        match t {
            0 => Some(Self::Reported),
            1 => Some(Self::InScope),
            2 => Some(Self::Human),
            _ => None,
        }
    }
}

/// What the scorer returned for one document's spans.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Emission<'a> {
    /// The prediction failed (no valid answer): no spans.
    PredictionFailed,
    /// The prediction succeeded and the artifact has no token head, so no
    /// emission ran (the plain class with spans requested).
    NotEmitted,
    /// The prediction succeeded and span emission failed (`span_status` is
    /// not `ok`).
    Failed,
    /// The decoded spans (possibly none).
    Spans(&'a [Span]),
    /// The prediction succeeded and the reply's span fields do not parse.
    Malformed,
}

/// A malformed span reply. The decoder is the runtime's code, never the
/// miner's, so any of these is the runtime's defect: the anchor check waives
/// the cohort's measured conditions instead of judging an entrant by it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FormatFault {
    /// Spans not in increasing start order.
    Unsorted,
    /// Two spans overlap.
    Overlap,
    /// A span is empty or ends past the text.
    Bounds,
    /// An offset is not on a character boundary.
    CharBoundary,
    /// `words` differs from the recount of the span's text.
    Words,
    /// A span under the floor while the decoder's minimum is at least the floor.
    UnderFloor,
    /// The reply's span fields do not parse.
    Malformed,
}

impl FormatFault {
    pub fn code(self) -> &'static str {
        match self {
            Self::Unsorted => "span.format.unsorted",
            Self::Overlap => "span.format.overlap",
            Self::Bounds => "span.format.bounds",
            Self::CharBoundary => "span.format.char_boundary",
            Self::Words => "span.format.words",
            Self::UnderFloor => "span.format.under_floor",
            Self::Malformed => "span.format.malformed",
        }
    }
}

/// The label data is unusable (a gold run outside the text or off a
/// character boundary, or a mixed document without one): a metrics error,
/// which waives the cohort's measured conditions and never holds anything.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LabelError(pub &'static str);

/// One batch document's span statistics ([`compute`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DocStats {
    pub population: Population,
    /// `M*` only: per-document character F1, ppb.
    pub f1_ppb: u32,
    /// `M*` only: `|P ∩ U|`.
    pub fp_human: u32,
    /// `M*` only: `|U|`.
    pub human: u32,
    /// `H*` only: a span decoded, a failed prediction, or a failed emission.
    pub flagged: bool,
    /// The prediction succeeded and span emission failed.
    pub emission_failed: bool,
    pub format_fault: Option<FormatFault>,
    /// Decoded spans (reported).
    pub spans: u32,
}

impl DocStats {
    /// The compact flag byte stored beside the counts: bit 0 flagged, bit 1
    /// emission failed, bit 2 format fault.
    pub fn flags(&self) -> u8 {
        u8::from(self.flagged) | (u8::from(self.emission_failed) << 1) | (u8::from(self.format_fault.is_some()) << 2)
    }
}

/// Whether byte offset `b` is a character boundary of `text` (the end counts).
fn on_boundary(text: &str, b: u32) -> bool {
    usize::try_from(b).is_ok_and(|b| text.is_char_boundary(b))
}

/// Checks a span reply against the text: sorted, non-overlapping, in bounds,
/// on character boundaries, each `words` equal to the recount, and none under
/// the floor when `decoder_min` is at least the floor.
pub fn check_spans(text: &str, spans: &[Span], decoder_min: Option<u32>) -> Option<FormatFault> {
    let len = u32::try_from(text.len()).unwrap_or(u32::MAX);
    let mut prev_start: Option<u32> = None;
    let mut prev_end = 0u32;
    for s in spans {
        if let Some(p) = prev_start {
            if s.start_byte <= p {
                return Some(FormatFault::Unsorted);
            }
            if s.start_byte < prev_end {
                return Some(FormatFault::Overlap);
            }
        }
        if s.start_byte >= s.end_byte || s.end_byte > len {
            return Some(FormatFault::Bounds);
        }
        if !on_boundary(text, s.start_byte) || !on_boundary(text, s.end_byte) {
            return Some(FormatFault::CharBoundary);
        }
        let body = &text[s.start_byte as usize..s.end_byte as usize];
        if words(body) != s.words {
            return Some(FormatFault::Words);
        }
        if decoder_min.is_some_and(|m| m >= FLOOR_WORDS) && s.words < FLOOR_WORDS {
            return Some(FormatFault::UnderFloor);
        }
        prev_start = Some(s.start_byte);
        prev_end = s.end_byte;
    }
    None
}

/// The gate population of a batch document from its label. A mixed document
/// needs a gold run inside the text on character boundaries.
pub fn population(text: &str, kind: Kind, gold: Option<(u32, u32)>) -> Result<Population, LabelError> {
    match kind {
        Kind::HumanOnly => Ok(Population::Human),
        Kind::Mixed => {
            let (a, b) = gold.ok_or(LabelError("mixed document without a gold run"))?;
            if a > b || !on_boundary(text, a) || !on_boundary(text, b) {
                return Err(LabelError("gold run outside the text or off a character boundary"));
            }
            if words(&text[a as usize..b as usize]) >= FLOOR_WORDS {
                Ok(Population::InScope)
            } else {
                Ok(Population::Reported)
            }
        }
        Kind::ModelOnly | Kind::Other => Ok(Population::Reported),
    }
}

/// Non-space character counts of a document against byte ranges: total,
/// inside `gold`, inside `p` (sorted, disjoint), and inside both.
fn counts(text: &str, gold: Option<(u32, u32)>, p: &[(u32, u32)]) -> (u64, u64, u64, u64) {
    let (mut total, mut g, mut pc, mut pg) = (0u64, 0u64, 0u64, 0u64);
    let mut at = 0usize;
    for (b, c) in text.char_indices() {
        if is_space(c) {
            continue;
        }
        let b = b as u32;
        total += 1;
        let in_g = gold.is_some_and(|(x, y)| b >= x && b < y);
        while at < p.len() && p[at].1 <= b {
            at += 1;
        }
        let in_p = at < p.len() && p[at].0 <= b && b < p[at].1;
        g += u64::from(in_g);
        pc += u64::from(in_p);
        pg += u64::from(in_g && in_p);
    }
    (total, g, pc, pg)
}

/// `floor(2 10^9 pg / (p + g))`, 0 when `p + g` is 0.
pub fn f1_ppb(pg: u64, p: u64, g: u64) -> u32 {
    let d = u128::from(p) + u128::from(g);
    if d == 0 {
        return 0;
    }
    let v = 2 * u128::from(PPB) * u128::from(pg) / d;
    u32::try_from(v.min(u128::from(PPB))).unwrap_or(u32::MAX)
}

/// One batch document's statistics. `decoder_min` is the artifact's decoder
/// minimum (from its load facts), used only by the format check.
pub fn compute(
    text: &str,
    kind: Kind,
    gold: Option<(u32, u32)>,
    emission: Emission<'_>,
    decoder_min: Option<u32>,
) -> Result<DocStats, LabelError> {
    let population = population(text, kind, gold)?;
    let mut out = DocStats { population, ..DocStats::default() };
    let mut ranges: Vec<(u32, u32)> = Vec::new();
    match emission {
        Emission::PredictionFailed => {}
        Emission::NotEmitted => {}
        Emission::Failed => out.emission_failed = true,
        Emission::Malformed => out.format_fault = Some(FormatFault::Malformed),
        Emission::Spans(spans) => {
            out.spans = u32::try_from(spans.len()).unwrap_or(u32::MAX);
            out.format_fault = check_spans(text, spans, decoder_min);
            if out.format_fault.is_none() {
                ranges.reserve_exact(spans.len());
                ranges.extend(spans.iter().map(|s| (s.start_byte, s.end_byte)));
            }
        }
    }
    match population {
        Population::InScope => {
            let (total, g, p, pg) = counts(text, gold, &ranges);
            out.f1_ppb = f1_ppb(pg, p, g);
            out.fp_human = u32::try_from(p - pg).unwrap_or(u32::MAX);
            out.human = u32::try_from(total - g).unwrap_or(u32::MAX);
        }
        Population::Human => {
            out.flagged = match emission {
                Emission::PredictionFailed | Emission::Failed | Emission::Malformed => true,
                Emission::NotEmitted => false,
                Emission::Spans(s) => !s.is_empty(),
            };
        }
        Population::Reported => {}
    }
    Ok(out)
}

/// A job's sums over its batch documents.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Agg {
    /// Batch documents with statistics.
    pub docs: u32,
    /// `|M*|`.
    pub in_scope: u32,
    pub f1_sum: u64,
    pub fp_human: u64,
    pub human: u64,
    /// `|H*|`.
    pub humans: u32,
    pub flagged: u32,
    pub emission_failed: u32,
    pub format_faults: u32,
    pub spans: u64,
}

impl Agg {
    pub fn add(&mut self, d: &DocStats) {
        self.docs += 1;
        match d.population {
            Population::InScope => {
                self.in_scope += 1;
                self.f1_sum += u64::from(d.f1_ppb);
                self.fp_human += u64::from(d.fp_human);
                self.human += u64::from(d.human);
            }
            Population::Human => {
                self.humans += 1;
                self.flagged += u32::from(d.flagged);
            }
            Population::Reported => {}
        }
        self.emission_failed += u32::from(d.emission_failed);
        self.format_faults += u32::from(d.format_fault.is_some());
        self.spans += u64::from(d.spans);
    }

    /// Mean F1 over `M*`, ppb (reported).
    pub fn mean_f1_ppb(&self) -> Option<u64> {
        (self.in_scope > 0).then(|| self.f1_sum / u64::from(self.in_scope))
    }

    /// The in-context rate `C = Σfp / Σhuman`, ppb (reported).
    pub fn in_context_ppb(&self) -> Option<u64> {
        (self.human > 0).then(|| (u128::from(self.fp_human) * u128::from(PPB) / u128::from(self.human)) as u64)
    }
}

/// The frozen margins of the measured conditions (the threshold record's).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Margins {
    /// `Δ_F1`, ppb of mean F1.
    pub f1_ppb: u32,
    /// `Δ_C`, ppb of the in-context rate.
    pub in_context_ppb: u32,
    /// `Δ_H`, human documents.
    pub humans: u32,
}

/// What the artifact itself declares: its token head and its decoder (from
/// the scorer's load facts). Zero hidden-data bits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Structure<'a> {
    pub token_head: bool,
    /// `(rule, min_words)` of the decoder, when the artifact has one.
    pub decoder: Option<(&'a str, u32)>,
}

/// Why an entrant fails the gate, in evaluation order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Reason {
    /// No token head.
    Missing,
    /// Not [`DECODER_RULE`], or its minimum is under the floor.
    Floor,
    /// More emission failures than allowed, on documents where the anchor emitted.
    Emission,
    /// Q1.
    F1,
    /// QC.
    InContext,
    /// QH.
    Humans,
}

impl Reason {
    pub const ALL: [Reason; 6] = [Reason::Missing, Reason::Floor, Reason::Emission, Reason::F1, Reason::InContext, Reason::Humans];

    /// The reason code (`GateFailure.reason`; private).
    pub fn code(self) -> &'static str {
        match self {
            Self::Missing => "span.missing",
            Self::Floor => "span.floor",
            Self::Emission => "span.emission",
            Self::F1 => "span.f1",
            Self::InContext => "span.in_context",
            Self::Humans => "span.humans",
        }
    }

    /// Whether the reason is a function of the artifact alone.
    pub fn structural(self) -> bool {
        matches!(self, Self::Missing | Self::Floor)
    }

    /// The public value of a failure: the structural facts by name, every
    /// hidden-data failure as one value.
    pub fn public(self) -> PublicValue {
        match self {
            Self::Missing => PublicValue::NoTokenHead,
            Self::Floor => PublicValue::DecoderUnderFloor,
            Self::Emission | Self::F1 | Self::InContext | Self::Humans => PublicValue::BelowAnchor,
        }
    }
}

/// What the published record says of one valued entrant.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PublicValue {
    Passed,
    NoTokenHead,
    DecoderUnderFloor,
    BelowAnchor,
}

impl PublicValue {
    pub fn code(self) -> &'static str {
        match self {
            Self::Passed => "passed",
            Self::NoTokenHead => "no_token_head",
            Self::DecoderUnderFloor => "decoder_under_floor",
            Self::BelowAnchor => "below_anchor",
        }
    }

    pub fn of(r: Option<Reason>) -> Self {
        r.map_or(Self::Passed, Reason::public)
    }
}

/// The structural conditions.
pub fn structural(s: &Structure<'_>) -> Option<Reason> {
    if !s.token_head {
        return Some(Reason::Missing);
    }
    match s.decoder {
        Some((rule, min)) if rule == DECODER_RULE && min >= FLOOR_WORDS => None,
        _ => Some(Reason::Floor),
    }
}

/// Entrant emission failures on batch documents where the anchor emitted:
/// `|E \ A|` for sorted, deduplicated task lists.
pub fn emission_excess(entrant: &[u32], anchor: &[u32]) -> u32 {
    let mut n = 0u32;
    let mut j = 0usize;
    for &t in entrant {
        while j < anchor.len() && anchor[j] < t {
            j += 1;
        }
        if !(j < anchor.len() && anchor[j] == t) {
            n += 1;
        }
    }
    n
}

/// The entrant and the anchor were not aggregated over the same documents.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Unpaired;

/// The measured conditions Q1, QC and QH, in order. The two aggregates must
/// cover the same documents (equal `|M*|`, `Σhuman` and `|H*|`).
pub fn measured(m: &Margins, entrant: &Agg, anchor: &Agg) -> Result<Option<Reason>, Unpaired> {
    if entrant.in_scope != anchor.in_scope || entrant.human != anchor.human || entrant.humans != anchor.humans {
        return Err(Unpaired);
    }
    let n = u128::from(entrant.in_scope);
    if u128::from(entrant.f1_sum) + u128::from(m.f1_ppb) * n < u128::from(anchor.f1_sum) {
        return Ok(Some(Reason::F1));
    }
    let scale = u128::from(PPB);
    if u128::from(entrant.fp_human) * scale > u128::from(anchor.fp_human) * scale + u128::from(m.in_context_ppb) * u128::from(entrant.human) {
        return Ok(Some(Reason::InContext));
    }
    if u64::from(entrant.flagged) > u64::from(anchor.flagged) + u64::from(m.humans) {
        return Ok(Some(Reason::Humans));
    }
    Ok(None)
}

/// One entrant's paired evaluation.
#[derive(Clone, Copy, Debug)]
pub struct Entrant<'a> {
    pub structure: Structure<'a>,
    pub agg: &'a Agg,
    /// [`emission_excess`] against the anchor.
    pub emission_excess: u32,
}

/// The gate: structural conditions, then (when `measured_applies`, i.e. the
/// cohort's anchor check passed and margins exist) the emission condition
/// and Q1, QC and QH against `anchor`. The first failing reason, or `None`.
pub fn verdict(
    e: &Entrant<'_>,
    anchor: &Agg,
    margins: Option<&Margins>,
    k_span: u32,
    measured_applies: bool,
) -> Result<Option<Reason>, Unpaired> {
    if let Some(r) = structural(&e.structure) {
        return Ok(Some(r));
    }
    let Some(m) = margins.filter(|_| measured_applies) else {
        return Ok(None);
    };
    if e.emission_excess > k_span {
        return Ok(Some(Reason::Emission));
    }
    measured(m, e.agg, anchor)
}

/// The advisory evaluation of the measured conditions, whether or not they
/// apply: what the gate would say with `margins` (reported privately).
pub fn advisory(e: &Entrant<'_>, anchor: &Agg, margins: &Margins, k_span: u32) -> Result<Option<Reason>, Unpaired> {
    verdict(e, anchor, Some(margins), k_span, true)
}

/// Why the cohort's measured conditions decide no entrant (the anchor
/// check), in check order. `Void` is the operator's.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Waiver {
    /// The operator voided the gate for this cohort before packaging.
    Void,
    /// (a) the anchor was not scored on the batch.
    AnchorMissing,
    /// (a) the anchor's emission failures exceed `k_span`.
    AnchorEmission,
    /// (a) a format fault in any job of the cohort.
    FormatFault,
    /// (b) a trivial policy passes Q1, QC and QH against the anchor.
    TrivialPasses,
    /// (d) `|M*|` under the measured lower quantile.
    InScopeCount,
    /// (e) a statistics computation error.
    MetricsError,
    /// (f) the population signature differs from the measured one.
    Population,
    /// Report mode without margins: nothing to apply.
    NoMargins,
}

impl Waiver {
    pub fn code(self) -> &'static str {
        match self {
            Self::Void => "void.operator",
            Self::AnchorMissing => "waived.anchor_missing",
            Self::AnchorEmission => "waived.anchor_emission",
            Self::FormatFault => "waived.format_fault",
            Self::TrivialPasses => "waived.trivial_passes",
            Self::InScopeCount => "waived.in_scope_count",
            Self::MetricsError => "waived.metrics_error",
            Self::Population => "waived.population",
            Self::NoMargins => "waived.no_margins",
        }
    }

    /// The public three-valued state (`waived` or `void`).
    pub fn state(self) -> CohortState {
        match self {
            Self::Void => CohortState::Void,
            _ => CohortState::Waived,
        }
    }
}

/// The public cohort state of the gate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CohortState {
    Applied,
    Waived,
    Void,
}

impl CohortState {
    pub fn code(self) -> &'static str {
        match self {
            Self::Applied => "applied",
            Self::Waived => "waived",
            Self::Void => "void",
        }
    }
}

/// The anchor check's inputs.
#[derive(Clone, Copy, Debug)]
pub struct AnchorCheck<'a> {
    pub void: bool,
    /// The anchor's aggregate on the batch, when it was scored.
    pub anchor: Option<&'a Agg>,
    pub k_span: u32,
    /// Any job of the cohort returned a malformed span.
    pub format_faults: u32,
    /// Some trivial policy passes every measured condition ([`trivial`]).
    pub trivial_passes: bool,
    pub in_scope_min: Option<u32>,
    pub metrics_error: bool,
    /// The population signature recomputed at seal equals the measured one
    /// (true when the block names none).
    pub population_ok: bool,
    pub margins: Option<&'a Margins>,
}

/// The anchor check: the first waiver, or `None` when the measured
/// conditions apply. It never holds a package.
pub fn anchor_check(c: &AnchorCheck<'_>) -> Option<Waiver> {
    if c.void {
        return Some(Waiver::Void);
    }
    if c.metrics_error {
        return Some(Waiver::MetricsError);
    }
    let Some(a) = c.anchor else {
        return Some(Waiver::AnchorMissing);
    };
    if a.emission_failed > c.k_span {
        return Some(Waiver::AnchorEmission);
    }
    if c.format_faults > 0 {
        return Some(Waiver::FormatFault);
    }
    if !c.population_ok {
        return Some(Waiver::Population);
    }
    if c.margins.is_none() {
        return Some(Waiver::NoMargins);
    }
    if c.trivial_passes {
        return Some(Waiver::TrivialPasses);
    }
    if c.in_scope_min.is_some_and(|m| a.in_scope < m) {
        return Some(Waiver::InScopeCount);
    }
    None
}

#[cfg(feature = "json")]
mod json;
#[cfg(feature = "json")]
pub use json::{SpanReply, span_reply};

#[cfg(test)]
mod tests;
