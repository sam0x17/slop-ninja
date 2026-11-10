//! What a failing entrant learns after the close: [`GateFeedbackV1`].
//!
//! A separate record signed with the package key beside a cohort's score
//! package and [`QuantizedResult`], for a plan that names it. It changes
//! neither: the package, the result, the manifest, the targets, the row and
//! their encodings are as before. It carries:
//!
//! - the cohort's gate thresholds ([`GateThresholds`]): the plan values the
//!   signed manifest binds by its gate-policy digest, which the record names,
//!   with whether they are provisional and whether the manifest enforces the
//!   span gate. They are plan parameters, fixed before any document is
//!   scored, and add no hidden-data bits;
//! - per roster entry, in roster order ([`EntrantFeedback`]): the result's
//!   not-executed reason (a copy of published content, or failed
//!   qualification: both decided at admission from public data), the
//!   withheld outcome (nothing more: the minimum-success rule publishes
//!   nothing else of a withheld entrant), or, for a scored entrant, the set
//!   of coarse gate classes it failed ([`GateClasses`]: share, attribution,
//!   the loose pure-human false-positive tier). No statistic, margin, count,
//!   reason code or per-document value appears. The public-copy gate keeps
//!   its own bit in the result, and the span gate its own record
//!   ([`crate::localization`]), which counts its per-entrant bit;
//! - its own hidden-data bits and the cohort's complete counted total, as
//!   the span gate's record does.
//!
//! **Bits.** Let `g` be the number of classes that can fail under the
//! thresholds ([`GateThresholds::can_fail`]). For a scored entrant the
//! result's gate bit is a function of the class set and the span gate's
//! value: it passes exactly when the set is empty and no span failure
//! applies ([`GateFeedbackV1::check_result`] refuses anything else). The
//! joint alphabet of the gate bit and the set is therefore `2^g`, of which
//! the result's budget already counts one bit, so the record's counted term
//! is `max(g - 1, 0)` bits per executed entrant and none per cohort
//! ([`terms`]), at full face value. A plan naming the record adds the term
//! to the one source every caller reads (`RewardV1Plan::counted_terms`), so
//! plan validation's worst case, the `live-v1` segment count and the
//! packaging recount all count it. On testnet's template (share and the
//! loose tier can fail, attribution cannot) that is one bit per executed
//! entrant.

use crate::codec::{Canonical, Decoder, Encoder, canonical_struct};
use crate::quant::{self, CountedTerms};
use crate::quantized_result::{EntrantOutcome, NotExecuted, QuantizedResult};
use crate::records::validate_label;
use crate::records_v2::{Gate, GateFailure, ScorePackageV2};
use crate::sign::KeyRole;
use crate::{ChainBinding, CohortManifestV2, Digest, Error, MAX_ROSTER, Record, Result};

/// The rule a plan names to publish the record.
pub const FEEDBACK_RULE: &str = "slop_ninja_gate_feedback_v1";

/// The largest share error and probability, in parts per billion.
const PPB_ONE: u32 = 1_000_000_000;

/// A set of coarse gate classes: the hidden-data gates a scored entrant
/// can fail other than the public-copy and span gates.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct GateClasses(u8);

impl GateClasses {
    pub const NONE: Self = Self(0);
    /// The share gate: mean absolute share error on mixed documents.
    pub const SHARE: Self = Self(1);
    /// The attribution gate: predictions carrying both role results.
    pub const ATTRIBUTION: Self = Self(2);
    /// The loose, per-entrant tier of the pure-human false-positive gate.
    pub const HUMAN_FPR: Self = Self(4);
    const ALL: u8 = 7;

    /// Every class with its public code, in gate order.
    pub const ORDER: [(Self, &'static str); 3] =
        [(Self::SHARE, "share"), (Self::ATTRIBUTION, "attribution"), (Self::HUMAN_FPR, "human_fpr")];

    pub fn bits(self) -> u8 {
        self.0
    }

    pub fn from_bits(bits: u8) -> Option<Self> {
        (bits & !Self::ALL == 0).then_some(Self(bits))
    }

    pub fn is_empty(self) -> bool {
        self.0 == 0
    }

    pub fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    pub fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// The number of classes in the set.
    pub fn count(self) -> u32 {
        self.0.count_ones()
    }

    /// The set's public codes, in gate order.
    pub fn codes(self) -> Vec<&'static str> {
        Self::ORDER.iter().filter(|(c, _)| self.contains(*c)).map(|(_, code)| *code).collect()
    }

    /// The class of a private gate, for the gates the set covers.
    pub fn of_gate(gate: Gate) -> Option<Self> {
        match gate {
            Gate::Share => Some(Self::SHARE),
            Gate::Attribution => Some(Self::ATTRIBUTION),
            Gate::HumanFalsePositive => Some(Self::HUMAN_FPR),
            Gate::Qualification | Gate::Span | Gate::PublicCopy | Gate::MinimumSuccess => None,
        }
    }

    /// The classes of an entrant's private gate failures (their reason
    /// codes are dropped).
    pub fn of_failures(failures: &[GateFailure]) -> Self {
        failures.iter().filter_map(|f| Self::of_gate(f.gate)).fold(Self::NONE, Self::union)
    }
}

impl Canonical for GateClasses {
    fn encode_to(&self, e: &mut Encoder) {
        e.u8(self.0);
    }
    fn decode_from(d: &mut Decoder<'_>) -> Result<Self> {
        let tag = d.u8()?;
        Self::from_bits(tag).ok_or(Error::InvalidTag { field: "gate_classes", tag })
    }
}

canonical_struct! {
    /// A cohort's gate thresholds: the plan's `reward_v1.gates` block and its
    /// provisional flag, which the signed manifest binds by its gate-policy
    /// digest, and the manifest's span-gate flag. Probabilities and share
    /// errors are in parts per billion, rates in per mille.
    #[derive(Copy)]
    pub struct GateThresholds {
        /// Testnet's provisional values, re-fixed from development evidence
        /// before live rewards.
        pub provisional: bool,
        /// A human-only document counts as a false positive when its
        /// involvement probability is at or above this operating point, or
        /// its prediction failed.
        pub operating_ppb: u32,
        /// The loose tier: at most this rate of the entrant's own cohort
        /// humans flagged.
        pub loose_fpr_max_permille: u32,
        /// The strict tier, a promotion condition rather than a gate.
        pub strict_fpr_max_permille: u32,
        /// The share gate: at most this mean absolute share error on mixed
        /// documents (a failed prediction counts as the largest error).
        pub share_mae_max_ppb: u32,
        /// The attribution gate: at least this share of the entrant's
        /// successful predictions carry both role results.
        pub attribution_min_permille: u32,
        /// Whether the manifest enforces the span gate. Its conditions and
        /// each entrant's span value are the span gate's record's.
        pub span_gate: bool,
    }
}

impl GateThresholds {
    /// The plan's ranges: an operating point in `(0, 1]`, rates at most
    /// 1000 per mille and a share error at most 1.
    pub fn validate(&self) -> Result<()> {
        if self.operating_ppb == 0
            || self.operating_ppb > PPB_ONE
            || self.loose_fpr_max_permille > 1000
            || self.strict_fpr_max_permille > 1000
            || self.share_mae_max_ppb > PPB_ONE
            || self.attribution_min_permille > 1000
        {
            return Err(Error::OutOfRange("gate thresholds"));
        }
        Ok(())
    }

    /// The classes an entrant can fail under these thresholds: share below
    /// the largest error, attribution above zero, and the loose tier below
    /// every human. A threshold no prediction can miss is not a class.
    pub fn can_fail(&self) -> GateClasses {
        let mut c = GateClasses::NONE;
        if self.share_mae_max_ppb < PPB_ONE {
            c = c.union(GateClasses::SHARE);
        }
        if self.attribution_min_permille > 0 {
            c = c.union(GateClasses::ATTRIBUTION);
        }
        if self.loose_fpr_max_permille < 1000 {
            c = c.union(GateClasses::HUMAN_FPR);
        }
        c
    }
}

/// The record's counted term under `thresholds`: `max(g - 1, 0)` bits per
/// executed entrant, `g` the classes that can fail, and none per cohort
/// (module docs).
pub fn terms(thresholds: &GateThresholds) -> CountedTerms {
    CountedTerms { per_executed: thresholds.can_fail().count().saturating_sub(1), per_cohort: 0 }
}

/// What the record publishes about one roster entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EntrantFeedback {
    /// Never executed, with the result's public reason.
    NotExecuted(NotExecuted),
    /// The minimum-success gate withheld it: nothing else.
    Withheld,
    /// Scored: the classes it failed, empty when none failed.
    Scored(GateClasses),
}

impl Canonical for EntrantFeedback {
    fn encode_to(&self, e: &mut Encoder) {
        match self {
            Self::NotExecuted(reason) => {
                e.u8(1);
                reason.encode_to(e);
            }
            Self::Withheld => e.u8(2),
            Self::Scored(classes) => {
                e.u8(3);
                classes.encode_to(e);
            }
        }
    }
    fn decode_from(d: &mut Decoder<'_>) -> Result<Self> {
        match d.u8()? {
            1 => Ok(Self::NotExecuted(NotExecuted::decode_from(d)?)),
            2 => Ok(Self::Withheld),
            3 => Ok(Self::Scored(GateClasses::decode_from(d)?)),
            tag => Err(Error::InvalidTag { field: "entrant_feedback", tag }),
        }
    }
}

impl EntrantFeedback {
    /// Executed: scored or withheld.
    pub fn executed(&self) -> bool {
        !matches!(self, Self::NotExecuted(_))
    }

    /// The failed classes of a scored entrant.
    pub fn classes(&self) -> Option<GateClasses> {
        match self {
            Self::Scored(c) => Some(*c),
            _ => None,
        }
    }
}

canonical_struct! {
    /// The signed public gate feedback of one scored cohort (module docs).
    pub struct GateFeedbackV1 {
        pub chain: ChainBinding,
        pub cohort: u64,
        pub cohort_label: String,
        pub cohort_manifest: Digest,
        /// The digest of the cohort's [`QuantizedResult`].
        pub result: Digest,
        /// The manifest's gate-policy digest: the policy document that holds
        /// these thresholds.
        pub gate_policy: Digest,
        pub thresholds: GateThresholds,
        /// Roster order.
        pub entrants: Vec<EntrantFeedback>,
        /// This record's hidden-data bits: [`terms`] at its executed count.
        pub bits: u32,
        /// The cohort's complete counted bits at the result's grid
        /// ([`crate::quant::cohort_bits`] under the plan's counted terms):
        /// the result's budget total plus every rule's term, this record's
        /// included.
        pub result_bits: u32,
        /// `n`, `h` and `F` of the result's budget check.
        pub cases: u32,
        pub identify_bits: u16,
        pub budget_factor: u16,
    }
}

impl GateFeedbackV1 {
    /// Executed entrants (scored or withheld).
    pub fn executed(&self) -> u16 {
        self.entrants.iter().filter(|e| e.executed()).count() as u16
    }

    /// The record's own bits for `executed` entrants under `thresholds`.
    pub fn count_bits(thresholds: &GateThresholds, executed: u16) -> Result<u32> {
        terms(thresholds).bits(executed)
    }

    /// The record of a finalized package and its public result: each scored
    /// entrant's classes from its private gate failures, everything else as
    /// the result shows it. `result_bits` is the cohort's complete counted
    /// total under the plan's terms. The record is validated and checked
    /// against the result before it is returned.
    pub fn of_package(
        package: &ScorePackageV2,
        result: &QuantizedResult,
        gate_policy: Digest,
        thresholds: GateThresholds,
        result_bits: u32,
    ) -> Result<Self> {
        if package.entrants.len() != result.entrants.len() {
            return Err(Error::Mismatch("feedback roster"));
        }
        let entrants = package
            .entrants
            .iter()
            .zip(&result.entrants)
            .map(|(p, r)| {
                if p.identity.hotkey != r.hotkey {
                    return Err(Error::Mismatch("feedback roster"));
                }
                Ok(match r.outcome {
                    EntrantOutcome::NotExecuted(reason) => EntrantFeedback::NotExecuted(reason),
                    EntrantOutcome::Withheld => EntrantFeedback::Withheld,
                    EntrantOutcome::Scored { .. } => EntrantFeedback::Scored(GateClasses::of_failures(&p.gate_failures)),
                })
            })
            .collect::<Result<Vec<_>>>()?;
        let executed = entrants.iter().filter(|e| e.executed()).count() as u16;
        let record = Self {
            chain: result.chain.clone(),
            cohort: result.cohort,
            cohort_label: result.cohort_label.clone(),
            cohort_manifest: result.cohort_manifest,
            result: result.digest()?,
            gate_policy,
            thresholds,
            bits: Self::count_bits(&thresholds, executed)?,
            entrants,
            result_bits,
            cases: result.cases,
            identify_bits: result.params.quant.identify_bits,
            budget_factor: result.params.quant.budget_factor,
        };
        record.validate()?;
        record.check_result(result)?;
        Ok(record)
    }

    /// The record against the cohort's result: the same cohort, manifest and
    /// roster, the result's digest and bit parameters, the result's outcome
    /// kind for every entrant (and its not-executed reason), a class set that
    /// is empty exactly when the result's gate bit passes (a failing entrant
    /// with no class only under an enforced span gate, whose own record gives
    /// its value), and a complete total that covers the result's budget plus
    /// this record's bits.
    pub fn check_result(&self, r: &QuantizedResult) -> Result<()> {
        if self.chain != r.chain
            || self.cohort != r.cohort
            || self.cohort_label != r.cohort_label
            || self.cohort_manifest != r.cohort_manifest
        {
            return Err(Error::Mismatch("feedback cohort"));
        }
        if self.result != r.digest()? {
            return Err(Error::Mismatch("feedback result digest"));
        }
        if self.entrants.len() != r.entrants.len() {
            return Err(Error::Mismatch("feedback roster"));
        }
        for (f, e) in self.entrants.iter().zip(&r.entrants) {
            match (f, e.outcome) {
                (EntrantFeedback::NotExecuted(a), EntrantOutcome::NotExecuted(b)) if *a == b => {}
                (EntrantFeedback::Withheld, EntrantOutcome::Withheld) => {}
                (EntrantFeedback::Scored(c), EntrantOutcome::Scored { gate_pass, .. }) => {
                    if gate_pass && !c.is_empty() {
                        return Err(Error::Invalid("feedback class for a passing entrant"));
                    }
                    if !gate_pass && c.is_empty() && !self.thresholds.span_gate {
                        return Err(Error::Invalid("feedback failing entrant without a class"));
                    }
                }
                _ => return Err(Error::Mismatch("feedback outcome")),
            }
        }
        if self.executed() != r.budget.executed {
            return Err(Error::Mismatch("feedback executed"));
        }
        let counted = self.bits.checked_add(r.budget.total).ok_or(Error::Overflow("counted bits"))?;
        if self.result_bits < counted
            || self.cases != r.cases
            || self.identify_bits != r.params.quant.identify_bits
            || self.budget_factor != r.params.quant.budget_factor
        {
            return Err(Error::Mismatch("feedback bit parameters"));
        }
        Ok(())
    }

    /// The record against the cohort's signed manifest: its digest, its
    /// gate-policy digest and its span-gate flag.
    pub fn check_manifest(&self, m: &CohortManifestV2) -> Result<()> {
        if self.cohort_manifest != m.digest()? || self.cohort != m.cohort || self.cohort_label != m.cohort_label {
            return Err(Error::Mismatch("feedback manifest"));
        }
        if self.gate_policy != m.gate_policy || self.thresholds.span_gate != m.span_gate {
            return Err(Error::Mismatch("feedback gate policy"));
        }
        Ok(())
    }

    /// The longest signed record: [`MAX_ROSTER`] scored entrants and the
    /// longest label. Readers refuse a larger declared length before
    /// allocating.
    pub fn max_signed_len() -> Result<u64> {
        let zero = Digest([0; 32]);
        let thresholds = GateThresholds {
            provisional: true,
            operating_ppb: 0,
            loose_fpr_max_permille: 0,
            strict_fpr_max_permille: 0,
            share_mae_max_ppb: 0,
            attribution_min_permille: 0,
            span_gate: false,
        };
        let longest = Self {
            chain: ChainBinding { genesis: zero, netuid: 0, mechanism: 0 },
            cohort: 0,
            cohort_label: "a".repeat(crate::MAX_LABEL_LEN),
            cohort_manifest: zero,
            result: zero,
            gate_policy: zero,
            thresholds,
            entrants: vec![EntrantFeedback::Scored(GateClasses::NONE); usize::from(MAX_ROSTER)],
            bits: u32::MAX,
            result_bits: u32::MAX,
            cases: u32::MAX,
            identify_bits: u16::MAX,
            budget_factor: u16::MAX,
        };
        let mut e = Encoder::new();
        e.str(Self::KIND);
        e.u16(Self::VERSION);
        longest.encode_to(&mut e);
        let record = e.into_bytes().len() as u64;
        // Envelope: str("signed") || u16 || u8 || u32 len || record || key || signature.
        Ok(4 + 6 + 2 + 1 + 4 + 32 + 64 + record)
    }
}

impl Record for GateFeedbackV1 {
    const KIND: &'static str = "gate-feedback";
    const VERSION: u16 = 1;
    const SIGNER: Option<KeyRole> = Some(KeyRole::Package);

    fn validate(&self) -> Result<()> {
        validate_label(&self.cohort_label)?;
        if self.entrants.len() > usize::from(MAX_ROSTER) {
            return Err(Error::LengthLimit { field: "feedback entrants", len: self.entrants.len() as u64 });
        }
        self.thresholds.validate()?;
        if self.cases == 0 || self.identify_bits == 0 || self.budget_factor == 0 {
            return Err(Error::OutOfRange("feedback bit parameters"));
        }
        if self.bits != Self::count_bits(&self.thresholds, self.executed())? {
            return Err(Error::Mismatch("feedback bits"));
        }
        // The complete total counts this record's bits, within `n h / F`.
        if self.bits > self.result_bits
            || !quant::within_bound(self.result_bits, self.cases, self.identify_bits, self.budget_factor)
        {
            return Err(Error::Invalid("feedback bits beyond n h / F"));
        }
        // Only a class the thresholds let fail is ever published.
        let can_fail = self.thresholds.can_fail();
        if self.entrants.iter().filter_map(EntrantFeedback::classes).any(|c| !can_fail.contains(c)) {
            return Err(Error::Invalid("feedback class the thresholds cannot fail"));
        }
        Ok(())
    }
}
