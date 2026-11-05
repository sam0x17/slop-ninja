//! What an outcome does to the item and to its vendor (design section 7).
//!
//! No retry loop exists for any terminal class: each is decided by status
//! and structured fields, never by counting repeats. A vendor's spend-cap,
//! quota or key answer pauses that vendor deployment-wide (caps are per
//! organization and month): its pending items move to Workers AI from their
//! streams' fallback decks ([`crate::rotation::fallback`]), recorded, and the
//! cohort keeps running; it never waits on the vendor. A pause clears only
//! when a probe (one fixed synthetic request to the vendor's smallest model
//! at its lowest effort, never cohort text) is answered, or by an operator
//! clear. No timer or calendar date resumes or stops anything.

use crate::outcome::OutcomeCode;
use crate::rotation::FallbackCause;
use crate::vendor::Vendor;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum VendorState {
    Active,
    Paused { cause: FallbackCause, since_ms: u64 },
}

/// The frontier vendors' states (the cohort object's `gen_vendor_state`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VendorStates {
    pub openai: VendorState,
    pub anthropic: VendorState,
}

impl Default for VendorStates {
    fn default() -> Self {
        VendorStates { openai: VendorState::Active, anthropic: VendorState::Active }
    }
}

impl VendorStates {
    pub fn get(&self, v: Vendor) -> VendorState {
        match v {
            Vendor::Openai => self.openai,
            Vendor::Anthropic => self.anthropic,
            Vendor::WorkersAi => VendorState::Active,
        }
    }
    fn slot(&mut self, v: Vendor) -> Option<&mut VendorState> {
        match v {
            Vendor::Openai => Some(&mut self.openai),
            Vendor::Anthropic => Some(&mut self.anthropic),
            Vendor::WorkersAi => None,
        }
    }
    /// The cause a draw sees for a paused vendor.
    pub fn paused(&self, v: Vendor) -> Option<FallbackCause> {
        match self.get(v) {
            VendorState::Active => None,
            VendorState::Paused { cause, .. } => Some(cause),
        }
    }
    /// Pauses `v` (a no-op when already paused: the first cause and time stay).
    /// Returns whether this call paused it.
    pub fn pause(&mut self, v: Vendor, cause: FallbackCause, now_ms: u64) -> bool {
        match self.slot(v) {
            Some(s @ VendorState::Active) => {
                *s = VendorState::Paused { cause, since_ms: now_ms };
                true
            }
            _ => false,
        }
    }
    /// A probe answered, or an operator clear: the vendor is active again.
    pub fn resume(&mut self, v: Vendor) -> bool {
        match self.slot(v) {
            Some(s @ VendorState::Paused { .. }) => {
                *s = VendorState::Active;
                true
            }
            _ => false,
        }
    }
}

/// What the cohort does after one call's outcome.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    /// Validate the text: accept, or content-reject and retry per the rotation.
    Validate,
    /// A content outcome without text: the attempt is used, retry per the rotation.
    RetryNextCell,
    /// No attempt used; not before the retry hint (else the throttle backoff).
    Throttle { retry_after_ms: Option<u64> },
    /// No attempt used; the outage backoff.
    Backoff,
    /// The item's frontier call moves to Workers AI (recorded `outage`).
    FallbackOutage,
    /// Pause the vendor and move its pending items to Workers AI.
    PauseVendor(FallbackCause),
    /// Disable the (model, effort) cell for the rest of the cohort (alert);
    /// the item retries on the next model of its lane, no attempt used.
    DisableCell,
}

/// The action for `outcome` of a call to `vendor`, the item having had
/// `outages` consecutive outages including this one. Flex unavailability is
/// handled inside the generation Worker (an immediate standard retry) and
/// never reaches here as a final outcome unless the standard retry was the
/// Flex answer's own.
pub fn decide(vendor: Vendor, outcome: OutcomeCode, outages: u32, failures_before_pause: u32, retry_after_ms: Option<u64>) -> Action {
    match outcome {
        OutcomeCode::Answer => Action::Validate,
        OutcomeCode::Refusal | OutcomeCode::LengthTokensOut | OutcomeCode::LengthBytes | OutcomeCode::ModelMismatch => Action::RetryNextCell,
        OutcomeCode::Throttle => Action::Throttle { retry_after_ms },
        OutcomeCode::FlexUnavailable => Action::Backoff,
        OutcomeCode::SpendCap if vendor != Vendor::WorkersAi => Action::PauseVendor(FallbackCause::SpendCap),
        OutcomeCode::KeyUnavailable if vendor != Vendor::WorkersAi => Action::PauseVendor(FallbackCause::KeyUnavailable),
        OutcomeCode::SpendCap | OutcomeCode::KeyUnavailable => Action::Backoff,
        OutcomeCode::CellRefused => Action::DisableCell,
        // A frontier item past the plan's outage bound moves to Workers AI
        // instead of pausing the cohort; Workers AI keeps today's rule.
        OutcomeCode::Outage if vendor != Vendor::WorkersAi && outages >= failures_before_pause => Action::FallbackOutage,
        OutcomeCode::Outage => Action::Backoff,
        // A deploy-order fault between the two Workers (alerted): nothing
        // reached a vendor; the item waits for the matching deploy.
        OutcomeCode::InterfaceMismatch => Action::Backoff,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::fixture::planned;
    use crate::rotation::{Avoid, StreamState, assign, fallback, stream_key};
    use crate::vendor::{DocKind, Lane};
    use crate::voice::fixture::library;

    #[test]
    fn a_spend_cap_pauses_the_vendor_and_moves_pending_items_to_workers_ai() {
        let m = planned();
        let lib = library();
        let seed = [2u8; 32];
        let s = stream_key(1, DocKind::Mixed, "wikipedia");
        let mut st = StreamState::default();
        let mut states = VendorStates::default();
        let mut pending: Vec<(String, crate::rotation::GenAssignment)> = (0..60)
            .map(|i| {
                let k = format!("d{i}");
                let a = assign(&mut st, &m, &lib, crate::voice::VoiceSplit::NoneHalf, &seed, &s, DocKind::Mixed, &k, &Avoid::default(), &|v| states.paused(v)).unwrap();
                (k, a)
            })
            .collect();
        // One Anthropic call answers with the spend cap.
        let action = decide(Vendor::Anthropic, OutcomeCode::SpendCap, 0, 10, None);
        assert_eq!(action, Action::PauseVendor(FallbackCause::SpendCap));
        assert!(states.pause(Vendor::Anthropic, FallbackCause::SpendCap, 1_000));
        assert!(!states.pause(Vendor::Anthropic, FallbackCause::KeyUnavailable, 2_000), "the first cause stays");
        // Every pending Anthropic item moves, recorded; the rest stay.
        let before: Vec<_> = pending.iter().map(|(_, a)| a.clone()).collect();
        for (_, a) in &mut pending {
            if m.model(&a.model).unwrap().vendor == Vendor::Anthropic {
                fallback(&mut st, &m, &seed, a, &Avoid::default(), Vendor::Anthropic, FallbackCause::SpendCap).unwrap();
            }
        }
        let mut moved = 0;
        for ((_, a), b) in pending.iter().zip(&before) {
            let was = m.model(&b.model).unwrap().vendor;
            if was == Vendor::Anthropic {
                moved += 1;
                assert_eq!(a.call_lane(), Lane::WorkersAi);
                assert_eq!((a.voice.clone(), a.operation.clone(), a.span_percent, a.intensity), (b.voice.clone(), b.operation.clone(), b.span_percent, b.intensity));
                assert_eq!(a.drawn().0, b.model);
            } else {
                assert_eq!(a, b);
            }
        }
        assert!(moved > 0);
        // Later draws of the paused vendor go to Workers AI as they are drawn.
        for i in 60..120 {
            let a = assign(&mut st, &m, &lib, crate::voice::VoiceSplit::NoneHalf, &seed, &s, DocKind::Mixed, &format!("d{i}"), &Avoid::default(), &|v| states.paused(v)).unwrap();
            assert_ne!(m.model(&a.model).unwrap().vendor, Vendor::Anthropic);
        }
        // OpenAI keeps running; a probe answer resumes Anthropic; nothing else does.
        assert_eq!(states.openai, VendorState::Active);
        assert!(states.resume(Vendor::Anthropic));
        assert_eq!(states.paused(Vendor::Anthropic), None);
        assert!(!states.resume(Vendor::WorkersAi));
    }

    #[test]
    fn no_terminal_class_loops() {
        for o in [OutcomeCode::SpendCap, OutcomeCode::KeyUnavailable] {
            assert!(matches!(decide(Vendor::Openai, o, 0, 10, None), Action::PauseVendor(_)));
        }
        assert_eq!(decide(Vendor::Openai, OutcomeCode::CellRefused, 0, 10, None), Action::DisableCell);
        assert_eq!(decide(Vendor::Anthropic, OutcomeCode::Outage, 9, 10, None), Action::Backoff);
        assert_eq!(decide(Vendor::Anthropic, OutcomeCode::Outage, 10, 10, None), Action::FallbackOutage);
        assert_eq!(decide(Vendor::WorkersAi, OutcomeCode::Outage, 50, 10, None), Action::Backoff);
        assert_eq!(decide(Vendor::Openai, OutcomeCode::Throttle, 0, 10, Some(3)), Action::Throttle { retry_after_ms: Some(3) });
        assert_eq!(decide(Vendor::Openai, OutcomeCode::Refusal, 0, 10, None), Action::RetryNextCell);
        let s = VendorStates { anthropic: VendorState::Paused { cause: FallbackCause::KeyUnavailable, since_ms: 5 }, ..Default::default() };
        let back: VendorStates = serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
        assert_eq!(back, s);
    }
}
