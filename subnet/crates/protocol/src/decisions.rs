//! Signed, content-free public records of operator decisions and of the
//! ends of cohorts that close without a target.
//!
//! Every record here carries codes, digests, cohort numbers and epochs only:
//! no text, label set, prediction or score beyond what is already public.
//! The eval Worker signs its records with the package key, beside the
//! cohort's other signed records; the coordinator signs its own with the
//! authorization key. A reader opens them under its pinned trust roots as
//! any other record ([`crate::Signed::open`]). A decision that also needed
//! the offline operator-decision key names the SHA-256 of the statement that
//! key signed ([`crate::operator::Statement::digest`]); the statement itself
//! (which names a decision id) stays with the operator.

use crate::codec::canonical_struct;
use crate::records::{ChainBinding, Record, bounded, validate_label};
use crate::sign::KeyRole;
use crate::submission::abandonment_code;
use crate::{Digest, Error, Result};

/// Longest action or phase name in a decision record.
pub const DECISION_NAME_MAX: usize = 64;

fn name(s: &str, what: &'static str) -> Result<()> {
    if s.is_empty()
        || s.len() > DECISION_NAME_MAX
        || !s.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"_-.:".contains(&b))
    {
        return Err(Error::Invalid(what));
    }
    Ok(())
}

canonical_struct! {
    /// A digest a decision names, under a short role name (`package`,
    /// `target`, `template`, `reference_current`, ...).
    pub struct NamedDigest {
        pub name: String,
        pub digest: Digest,
    }
}

canonical_struct! {
    /// The abandonment of a cohort, written where version 1
    /// ([`crate::submission::CohortAbandonment`]) is, for an abandonment by
    /// operator decision. Beyond version 1 it names the phase the cohort was
    /// in and the package, result and target digests that existed then
    /// (an abandonment in packaging or the transition declines an outcome
    /// that may already be known), whether it came by decision, and the
    /// decision's digests. A cohort with no fault publishes fault `none`.
    pub struct CohortAbandonmentV2 {
        pub chain: ChainBinding,
        pub cohort: u64,
        pub cohort_label: String,
        pub cohort_manifest: Option<Digest>,
        pub fault: String,
        pub reason: String,
        pub abandoned_epoch: Option<u64>,
        pub phase_at_abandon: String,
        pub score_package: Option<Digest>,
        pub result: Option<Digest>,
        pub target: Option<Digest>,
        pub operator: bool,
        pub decision: Option<Digest>,
        /// SHA-256 of the offline-signed statement, for an abandonment in
        /// packaging or the transition.
        pub statement: Option<Digest>,
    }
}

impl Record for CohortAbandonmentV2 {
    const KIND: &'static str = "cohort-abandonment";
    const VERSION: u16 = 2;
    const SIGNER: Option<KeyRole> = Some(KeyRole::Package);
    fn validate(&self) -> Result<()> {
        validate_label(&self.cohort_label)?;
        abandonment_code(&self.fault, "abandonment fault")?;
        abandonment_code(&self.reason, "abandonment reason")?;
        name(&self.phase_at_abandon, "abandonment phase")
    }
}

canonical_struct! {
    /// The operator declined a faulted closure: the cohort closed, its
    /// closure record was never written, and it never will be. Its target
    /// is never installed. Signed with the package key under the cohort's
    /// receipts prefix; the coordinator's handoff passes the number by it.
    pub struct ClosureDeclined {
        pub chain: ChainBinding,
        pub cohort: u64,
        pub cohort_label: String,
        pub cohort_manifest: Option<Digest>,
        pub score_package: Option<Digest>,
        /// The target the closure would have activated, if any.
        pub declined_target: Option<Digest>,
        pub phase: String,
        pub fault: String,
        pub declined_epoch: Option<u64>,
        pub decision: Option<Digest>,
        pub statement: Digest,
    }
}

impl Record for ClosureDeclined {
    const KIND: &'static str = "closure-declined";
    const VERSION: u16 = 1;
    const SIGNER: Option<KeyRole> = Some(KeyRole::Package);
    fn validate(&self) -> Result<()> {
        validate_label(&self.cohort_label)?;
        name(&self.phase, "declined phase")?;
        abandonment_code(&self.fault, "declined fault")
    }
}

canonical_struct! {
    /// A probe cut consumed this cohort number and closed without a target:
    /// a probe build sits outside the public record, so it has no manifest
    /// under the record's prefixes. `cut_epoch` is the finalized epoch index
    /// at which the cut was recorded.
    pub struct ProbeCutRecord {
        pub chain: ChainBinding,
        pub cohort: u64,
        pub cohort_label: String,
        pub probe_version: u32,
        pub cut_epoch: u64,
    }
}

impl Record for ProbeCutRecord {
    const KIND: &'static str = "cohort-probe-cut";
    const VERSION: u16 = 1;
    const SIGNER: Option<KeyRole> = Some(KeyRole::Package);
    fn validate(&self) -> Result<()> {
        validate_label(&self.cohort_label)
    }
}

canonical_struct! {
    /// Any other eval operator decision that changes reward inputs, payouts
    /// or what is published (a reference decision, a closure rewrite, an
    /// operator `withheld`, a cohort started from an inline template).
    pub struct EvalDecision {
        pub chain: ChainBinding,
        pub authority: String,
        pub action: String,
        pub reason: String,
        pub cohort: Option<u64>,
        pub cohort_label: Option<String>,
        pub digests: Vec<NamedDigest>,
        pub epoch: Option<u64>,
        pub decision: Option<Digest>,
        pub statement: Option<Digest>,
    }
}

impl Record for EvalDecision {
    const KIND: &'static str = "eval-decision";
    const VERSION: u16 = 1;
    const SIGNER: Option<KeyRole> = Some(KeyRole::Package);
    fn validate(&self) -> Result<()> {
        name(&self.authority, "decision authority")?;
        name(&self.action, "decision action")?;
        abandonment_code(&self.reason, "decision reason")?;
        if let Some(l) = &self.cohort_label {
            validate_label(l)?;
        }
        for d in &self.digests {
            name(&d.name, "decision digest name")?;
        }
        bounded(&self.digests, "decision digests")
    }
}

canonical_struct! {
    /// One cohort a coordinator restart passed without installing it.
    pub struct RestartPassed {
        pub cohort: u64,
        /// What the coordinator knew of it: `pending`, `closed`, `in_record`,
        /// `unsealed` or `unlisted`.
        pub state: String,
    }
}

canonical_struct! {
    /// The coordinator switched to burn ("burn now"): with signing paused it
    /// set the held target aside and installed the launch burn row from
    /// epoch `floor`, under a launch authorization issued at that epoch.
    /// `past` names every cohort known to have opened before the floor that
    /// it had not handled; `unlisted` every number up to `last` (the highest
    /// opened number the operator read) that was not listed then, so a
    /// pre-floor cohort found later is named in a signed record either way.
    /// `statement` is the offline-signed statement's SHA-256; `None` for a
    /// restart made while the coordinator had no operator-decision key
    /// configured (a token-level restart). Signed with the authorization key.
    pub struct ChainRestart {
        pub chain: ChainBinding,
        pub floor: u64,
        pub launch_authorization: Digest,
        pub previous_target: Option<Digest>,
        pub previous_activated_epoch: Option<u64>,
        pub last: Option<u64>,
        pub past: Vec<RestartPassed>,
        pub unlisted: Vec<u64>,
        pub decision: Option<Digest>,
        pub statement: Option<Digest>,
    }
}

impl Record for ChainRestart {
    const KIND: &'static str = "chain-restart";
    const VERSION: u16 = 1;
    const SIGNER: Option<KeyRole> = Some(KeyRole::Authorization);
    fn validate(&self) -> Result<()> {
        for p in &self.past {
            name(&p.state, "restart cohort state")?;
        }
        bounded(&self.past, "restart past")?;
        bounded(&self.unlisted, "restart unlisted")
    }
}

canonical_struct! {
    /// A cohort found after a restart that opened before its floor and that
    /// the restart's signed record did not name (`restarted_past_unlisted`):
    /// an addendum to that [`ChainRestart`], signed with the authorization
    /// key.
    pub struct RestartAddendum {
        pub chain: ChainBinding,
        pub floor: u64,
        pub restart: Digest,
        pub cohort: u64,
        pub cohort_label: String,
        pub opened_epoch: u64,
    }
}

impl Record for RestartAddendum {
    const KIND: &'static str = "chain-restart-addendum";
    const VERSION: u16 = 1;
    const SIGNER: Option<KeyRole> = Some(KeyRole::Authorization);
    fn validate(&self) -> Result<()> {
        validate_label(&self.cohort_label)
    }
}

/// An ended cohort's signed end record of either abandonment version.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AbandonmentView {
    pub chain: ChainBinding,
    pub cohort: u64,
    pub cohort_label: String,
    pub cohort_manifest: Option<Digest>,
    pub abandoned_epoch: Option<u64>,
    pub phase_at_abandon: Option<String>,
    pub operator: bool,
}

/// Opens an abandonment record of version 2 or version 1 under `keys`.
pub fn open_abandonment(bytes: &[u8], keys: &crate::TrustedKeys) -> Result<AbandonmentView> {
    match crate::Signed::<CohortAbandonmentV2>::open(bytes, keys) {
        Ok(r) => Ok(AbandonmentView {
            chain: r.chain,
            cohort: r.cohort,
            cohort_label: r.cohort_label,
            cohort_manifest: r.cohort_manifest,
            abandoned_epoch: r.abandoned_epoch,
            phase_at_abandon: Some(r.phase_at_abandon),
            operator: r.operator,
        }),
        // The signature covers the version's own domain, so a version-1
        // record fails version 2's check before it decodes: try version 1.
        Err(e2) => {
            let Ok(r) = crate::Signed::<crate::submission::CohortAbandonment>::open(bytes, keys) else {
                return Err(e2);
            };
            Ok(AbandonmentView {
                chain: r.chain,
                cohort: r.cohort,
                cohort_label: r.cohort_label,
                cohort_manifest: r.cohort_manifest,
                abandoned_epoch: r.abandoned_epoch,
                phase_at_abandon: None,
                operator: false,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures;

    fn chain() -> ChainBinding {
        ChainBinding { genesis: crate::TESTNET_GENESIS, netuid: 601, mechanism: 0 }
    }

    #[test]
    fn both_abandonment_versions_open() {
        let keys = fixtures::trusted_keys();
        let v1 = crate::submission::CohortAbandonment {
            chain: chain(),
            cohort: 7,
            cohort_label: "c7".into(),
            cohort_manifest: None,
            fault: "plan_defect".into(),
            reason: "drill".into(),
            abandoned_epoch: Some(3),
        };
        let b1 = crate::sign(v1, &fixtures::package_key()).unwrap().to_bytes().unwrap();
        let view = open_abandonment(&b1, &keys).unwrap();
        assert_eq!((view.cohort, view.operator, view.phase_at_abandon), (7, false, None));
        let v2 = CohortAbandonmentV2 {
            chain: chain(),
            cohort: 8,
            cohort_label: "c8".into(),
            cohort_manifest: None,
            fault: "none".into(),
            reason: "operator_decision".into(),
            abandoned_epoch: Some(4),
            phase_at_abandon: "queued".into(),
            score_package: None,
            result: None,
            target: None,
            operator: true,
            decision: Some(Digest([1; 32])),
            statement: None,
        };
        let b2 = crate::sign(v2, &fixtures::package_key()).unwrap().to_bytes().unwrap();
        let view = open_abandonment(&b2, &keys).unwrap();
        assert_eq!((view.cohort, view.operator, view.phase_at_abandon.as_deref()), (8, true, Some("queued")));
        // The receipt key's signature is refused.
        let bad = crate::sign(
            CohortAbandonmentV2 { cohort: 9, ..crate::Signed::<CohortAbandonmentV2>::open(&b2, &keys).unwrap() },
            &fixtures::receipt_key(),
        )
        .unwrap()
        .to_bytes()
        .unwrap();
        assert!(open_abandonment(&bad, &keys).is_err());
    }

    #[test]
    fn records_refuse_free_text() {
        let r = EvalDecision {
            chain: chain(),
            authority: "r1".into(),
            action: "references.decision".into(),
            reason: "span equivalent".into(),
            cohort: None,
            cohort_label: None,
            digests: vec![],
            epoch: None,
            decision: None,
            statement: None,
        };
        assert!(r.validate().is_err());
        let restart = ChainRestart {
            chain: chain(),
            floor: 10,
            launch_authorization: Digest([2; 32]),
            previous_target: None,
            previous_activated_epoch: None,
            last: Some(4),
            past: vec![RestartPassed { cohort: 3, state: "closed".into() }],
            unlisted: vec![4],
            decision: None,
            statement: Some(Digest([3; 32])),
        };
        assert!(restart.validate().is_ok());
    }
}
