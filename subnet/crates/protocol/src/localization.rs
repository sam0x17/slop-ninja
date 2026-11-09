//! The span gate's public outcome: [`LocalizationOutcomeV1`].
//!
//! A separate record signed with the package key beside a cohort's score
//! package and [`QuantizedResult`]. It changes neither: the package, the
//! result, the manifest, the targets and their encodings are as before. It
//! carries, after close, one value per executed entrant that is not withheld
//! ([`LocalizationValue`]) and the cohort's three-valued gate state, and it
//! states its own hidden-data bits and the cohort's complete counted total:
//!
//! - one bit per valued entrant (passed or below the anchor; the structural
//!   values `no_token_head` and `decoder_under_floor` are facts of the
//!   artifact, public from scoring start, and are counted anyway);
//! - [`STATE_BITS`] per cohort for the state (`ceil(log2 3)`).
//!
//! These bits are a counted term of the cohort's budget ([`TERMS`], a
//! [`CountedTerms`]): a plan naming the gate adds the term to the one source
//! every caller reads, so plan validation's worst case, the `live-v1`
//! segment count ([`crate::live::derive_segments`]) and the packaging
//! recount ([`crate::quant::cohort_bits`]) all count it. The record's
//! `result_bits` is that complete total at the result's grid, which must
//! cover the result's budget plus the term and stay within `n h / F`.
//!
//! A withheld or unexecuted entrant gets no value: the minimum-success rule
//! publishes nothing of a withheld entrant but its identity, and a span value
//! would leak hidden-data bits for exactly the entrants that rule protects.
//! [`LocalizationOutcomeV1::check_result`] refuses such a value.
//!
//! In report mode the record is advisory: no gate failure of the package
//! comes from it. Only an applied state carries `below_anchor`: when the
//! cohort is waived or void, the measured conditions decide no entrant.

use crate::codec::canonical_struct;
use crate::quant::{self, CountedTerms};
use crate::quantized_result::{EntrantOutcome, QuantizedResult};
use crate::records::validate_label;
use crate::records_v2::tagged_enum;
use crate::sign::KeyRole;
use crate::{ChainBinding, Digest, Error, MAX_ROSTER, Record, Result};

/// The gate rule the record reports.
pub const SPAN_GATE_RULE: &str = "slop_ninja_span_gate_v1";

/// Reason codes of span-gate failures (`GateFailure.reason` under
/// `Gate::Span`; private), in evaluation order.
pub mod reasons {
    pub const MISSING: &str = "span.missing";
    pub const FLOOR: &str = "span.floor";
    pub const EMISSION: &str = "span.emission";
    pub const F1: &str = "span.f1";
    pub const IN_CONTEXT: &str = "span.in_context";
    pub const HUMANS: &str = "span.humans";
    pub const ALL: [&str; 6] = [MISSING, FLOOR, EMISSION, F1, IN_CONTEXT, HUMANS];
}

tagged_enum! {
    /// Whether the cohort's manifest enforces the span gate.
    pub enum LocalizationMode ("localization_mode") {
        /// Advisory: computed and published, failing no entrant.
        Report = 1,
        Enforce = 2,
    }
}

tagged_enum! {
    /// The cohort's gate state.
    pub enum LocalizationState ("localization_state") {
        /// The measured conditions applied.
        Applied = 1,
        /// The anchor check failed: the measured conditions decided no entrant.
        Waived = 2,
        /// The operator voided the gate for this cohort before packaging.
        Void = 3,
    }
}

tagged_enum! {
    /// One valued entrant's public span outcome.
    pub enum LocalizationValue ("localization_value") {
        Passed = 1,
        NoTokenHead = 2,
        DecoderUnderFloor = 3,
        /// Any hidden-data failure: emission, character F1, in-context
        /// false positives or human documents with a span.
        BelowAnchor = 4,
    }
}

impl LocalizationValue {
    pub fn code(self) -> &'static str {
        match self {
            Self::Passed => "passed",
            Self::NoTokenHead => "no_token_head",
            Self::DecoderUnderFloor => "decoder_under_floor",
            Self::BelowAnchor => "below_anchor",
        }
    }
}

impl LocalizationState {
    pub fn code(self) -> &'static str {
        match self {
            Self::Applied => "applied",
            Self::Waived => "waived",
            Self::Void => "void",
        }
    }
}

impl LocalizationMode {
    pub fn code(self) -> &'static str {
        match self {
            Self::Report => "report",
            Self::Enforce => "enforce",
        }
    }
}

/// Bits of the cohort state under the budget's `ceil(log2(alphabet))` rule.
pub const STATE_BITS: u32 = 2;

/// The record's counted term: one bit per executed entrant (at least the
/// valued ones: only a scored entrant gets a value) and [`STATE_BITS`] per
/// cohort, so `X + 2` at `X` executed entrants. A plan naming the gate
/// returns it from its counted terms, never adding it at a caller.
pub const TERMS: CountedTerms = CountedTerms { per_executed: 1, per_cohort: STATE_BITS };

canonical_struct! {
    /// The signed public span outcome of one scored cohort (module docs).
    pub struct LocalizationOutcomeV1 {
        pub chain: ChainBinding,
        pub cohort: u64,
        pub cohort_label: String,
        pub cohort_manifest: Digest,
        /// The digest of the cohort's [`QuantizedResult`].
        pub result: Digest,
        pub mode: LocalizationMode,
        /// The digest of the gate policy the manifest binds.
        pub gate_policy: Digest,
        /// The fixed anchor's inference content.
        pub anchor_content: Digest,
        pub state: LocalizationState,
        /// Roster order; `None` for an entrant with no value.
        pub entrants: Vec<Option<LocalizationValue>>,
        /// This record's hidden-data bits: one per valued entrant plus [`STATE_BITS`].
        pub bits: u32,
        /// The cohort's complete counted bits at the result's grid
        /// ([`crate::quant::cohort_bits`] under the plan's counted terms):
        /// the result's budget total plus every rule's term, this record's
        /// [`TERMS`] included.
        pub result_bits: u32,
        /// `n`, `h` and `F` of the result's budget check.
        pub cases: u32,
        pub identify_bits: u16,
        pub budget_factor: u16,
    }
}

impl LocalizationOutcomeV1 {
    /// The record's own bits for its values.
    pub fn count_bits(entrants: &[Option<LocalizationValue>]) -> u32 {
        entrants.iter().filter(|v| v.is_some()).count() as u32 + STATE_BITS
    }

    /// The record against the cohort's result: the same cohort, manifest and
    /// roster, the result's digest and bit parameters, a complete total that
    /// covers the result's budget plus [`TERMS`] at its executed count, and
    /// no value for an entrant the result shows withheld or not executed.
    pub fn check_result(&self, r: &QuantizedResult) -> Result<()> {
        if self.chain != r.chain
            || self.cohort != r.cohort
            || self.cohort_label != r.cohort_label
            || self.cohort_manifest != r.cohort_manifest
        {
            return Err(Error::Mismatch("localization cohort"));
        }
        if self.result != r.digest()? {
            return Err(Error::Mismatch("localization result digest"));
        }
        if self.entrants.len() != r.entrants.len() {
            return Err(Error::Mismatch("localization roster"));
        }
        for (v, e) in self.entrants.iter().zip(&r.entrants) {
            if v.is_some() && !matches!(e.outcome, EntrantOutcome::Scored { .. }) {
                return Err(Error::Invalid("localization value for a withheld or unexecuted entrant"));
            }
        }
        let counted = TERMS.bits(r.budget.executed)?.checked_add(r.budget.total).ok_or(Error::Overflow("counted bits"))?;
        if self.result_bits < counted
            || self.cases != r.cases
            || self.identify_bits != r.params.quant.identify_bits
            || self.budget_factor != r.params.quant.budget_factor
        {
            return Err(Error::Mismatch("localization bit parameters"));
        }
        Ok(())
    }
}

impl Record for LocalizationOutcomeV1 {
    const KIND: &'static str = "localization-outcome";
    const VERSION: u16 = 1;
    const SIGNER: Option<KeyRole> = Some(KeyRole::Package);

    fn validate(&self) -> Result<()> {
        validate_label(&self.cohort_label)?;
        if self.entrants.len() > usize::from(MAX_ROSTER) {
            return Err(Error::LengthLimit {
                field: "localization entrants",
                len: self.entrants.len() as u64,
            });
        }
        if self.cases == 0 || self.identify_bits == 0 || self.budget_factor == 0 {
            return Err(Error::OutOfRange("localization bit parameters"));
        }
        if self.bits != Self::count_bits(&self.entrants) {
            return Err(Error::Mismatch("localization bits"));
        }
        // The complete total counts this record's bits, within `n h / F`.
        if self.bits > self.result_bits
            || !quant::within_bound(self.result_bits, self.cases, self.identify_bits, self.budget_factor)
        {
            return Err(Error::Invalid("localization bits beyond n h / F"));
        }
        // Waived or void: the measured conditions decide no entrant.
        if self.state != LocalizationState::Applied && self.entrants.contains(&Some(LocalizationValue::BelowAnchor)) {
            return Err(Error::Invalid("localization below_anchor outside an applied state"));
        }
        Ok(())
    }
}
