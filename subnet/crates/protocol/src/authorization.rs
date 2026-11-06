//! The validators' reward-intent authorizations and their signed revocation.
//!
//! Two records authorize new weight commitments, both signed under
//! [`KeyRole::Authorization`]:
//!
//! - A [`LaunchAuthorization`] names only the chain and the launch burn row,
//!   with no manifest, so validators commit the burn row from the first tempo
//!   of the first cohort, before any manifest is sealed. It serves until the
//!   first cohort target activates at its closing boundary. Between the signing
//!   of the first package and that boundary the new target is pending and the
//!   burn row is still the active target, so the launch authorization keeps
//!   validators committing without a gap, and it can only ever commit the burn
//!   row, which pays no miner. From that boundary every validator refuses it,
//!   at its check and at every per-tempo operation, so the burn row is never
//!   re-authorized after a cohort target has been active.
//! - A score package that produced a target is followed by a per-cohort
//!   [`RewardIntentAuthorization`] that names it and authorizes that target,
//!   whose predecessor must be the validator's active target unless its
//!   closure has already activated it. For a cohort that produced none, the
//!   record form also admits an authorization of the target already active.
//!
//! **Nothing expires.** An authorization yields commitments from its
//! `issued_epoch` for as long as its target is the validator's active target,
//! with no end epoch: it stops only when a newer authorization supersedes it
//! (its cohort's target activates, so the old target is no longer active) or a
//! signed [`RewardIntentRevocation`] withdraws it. A silent coordinator
//! therefore never stops validators from committing the last authorized
//! target. Each record still carries `renew_by_epoch`, a non-binding hint of
//! when the issuer meant to publish a fresher record (the launch record's is
//! `issued_epoch +` [`LAUNCH_RENEW_BY_TEMPOS`], a cohort record's the
//! manifest's frozen `lease_tempos` later); it is checked as part of the
//! record's format and never refuses or pauses a commitment. A
//! [`RewardIntentRenewal`] only moves that hint; nothing requires one.
//!
//! A [`RewardIntentRevocation`] names the chain and a target and withdraws
//! every authorization of that target issued at or before its own
//! `issued_epoch`; an authorization of the same target issued later
//! re-authorizes it. A validator journals every checked revocation and passes
//! them to each per-tempo operation, which refuses a withdrawn authorization
//! (`revoked`). A journaled revocation is not verified again
//! ([`RewardIntentRevocation::journaled`]): it can only withdraw, and only
//! authorizations of its own chain and target, so a rotated trust root never
//! turns an old revocation into a stop for other targets. Its signer bounds
//! `issued_epoch` by the finalized epoch index when it signs or admits one,
//! because an authorization that re-authorizes the target must be issued
//! later and cannot commit before its own issue. Revocation is the only way to stop an authorized target
//! short of supersession, and it is explicit and signed.
//!
//! Checking takes the signed envelope, never the bare record:
//! [`LaunchAuthorization::check`] and [`RewardIntentAuthorization::check`]
//! verify the signature under the trusted authorization key and return a
//! [`CheckedAuthorization`]; [`RewardIntentRevocation::check`] returns a
//! [`CheckedRevocation`]. Only a checked authorization yields a new per-tempo
//! commitment ([`CheckedAuthorization::operation`]), with the epoch index read
//! from a fresh finalized view of the chain, the validator's [`ActiveTarget`]
//! as of that tempo and its checked revocations. The operation re-checks the
//! active target every time: the authorized target must be the active one, the
//! epoch at or after its activation (the closing epoch of its
//! [`CheckedClosure`]; the launch burn row has none) and at or after the
//! authorization's issue, and no revocation may withdraw it. An authorization
//! checked earlier therefore stops yielding commitments as soon as another
//! target activates. Commitments already submitted keep their recorded
//! authority and native deadlines; reconciling them recomputes their identity
//! with [`WeightOperationV2::for_target`].
//!
//! A package's new target digest covers the package digest, so the
//! authorization names the package and its target rather than the package
//! embedding the authorization.

use crate::codec::canonical_struct;
use crate::records_v2::{
    CalculationTargetV2, CohortManifestV2, CutoffMode, ScorePackageV2, TargetSource,
};
use crate::sign::{KeyRole, Signed, TrustedKeys};
use crate::submission::CheckedClosure;
use crate::{AccountId, ChainBinding, Digest, Error, Record, Result, WeightOperationV2};

/// Distance of a [`LaunchAuthorization`]'s non-binding `renew_by_epoch` hint
/// from its issue: 40 tempos, two launch cohorts. Part of the record's format
/// only; no authorization expires.
pub const LAUNCH_RENEW_BY_TEMPOS: u64 = 40;

canonical_struct! {
    /// The manifest-free authorization of the launch burn row, valid while no
    /// cohort target has activated: through the signing of the first package
    /// and until that package's target activates at its closing boundary.
    pub struct LaunchAuthorization {
        pub chain: ChainBinding,
        /// Digest of [`CalculationTargetV2::launch_burn`] on `chain`.
        pub target: Digest,
        /// Generation of the coordinator checkpoint that issued it.
        pub checkpoint_generation: u64,
        /// First epoch index at which it authorizes a commitment.
        pub issued_epoch: u64,
        /// `issued_epoch + LAUNCH_RENEW_BY_TEMPOS`: a non-binding hint, never
        /// an expiry (module docs).
        pub renew_by_epoch: u64,
    }
}

impl Record for LaunchAuthorization {
    const KIND: &'static str = "launch-authorization";
    const VERSION: u16 = 1;
    const SIGNER: Option<KeyRole> = Some(KeyRole::Authorization);
    fn validate(&self) -> Result<()> {
        if self.renew_by_epoch <= self.issued_epoch {
            return Err(Error::Invalid("authorization renew-by hint"));
        }
        Ok(())
    }
}

canonical_struct! {
    /// Per-cohort reward-intent authorization for the target active after a
    /// score package.
    pub struct RewardIntentAuthorization {
        pub chain: ChainBinding,
        /// The cohort it was issued for.
        pub cohort: u64,
        pub cohort_manifest: Digest,
        /// The score package it names.
        pub score_package: Digest,
        /// Digest of the target active after the package: new or carried over.
        pub target: Digest,
        /// Generation of the coordinator checkpoint that issued it.
        pub checkpoint_generation: u64,
        /// First epoch index at which it authorizes a commitment.
        pub issued_epoch: u64,
        /// `issued_epoch` plus the manifest's `lease_tempos`: a non-binding
        /// hint, never an expiry (module docs).
        pub renew_by_epoch: u64,
    }
}

impl Record for RewardIntentAuthorization {
    const KIND: &'static str = "reward-intent-authorization";
    const VERSION: u16 = 2;
    const SIGNER: Option<KeyRole> = Some(KeyRole::Authorization);
    fn validate(&self) -> Result<()> {
        if self.renew_by_epoch <= self.issued_epoch {
            return Err(Error::Invalid("authorization renew-by hint"));
        }
        Ok(())
    }
}

canonical_struct! {
    /// A refresh of a reward-intent or launch authorization's non-binding
    /// renew-by hint. It leaves the target unchanged, and nothing requires
    /// one: an authorization never expires.
    pub struct RewardIntentRenewal {
        /// Digest of the authorization it renews.
        pub authorization: Digest,
        /// Never below the authorization's.
        pub checkpoint_generation: u64,
        /// Never before the authorization's.
        pub issued_epoch: u64,
        /// `issued_epoch` plus the authorization's renew-by distance.
        pub renew_by_epoch: u64,
    }
}

impl Record for RewardIntentRenewal {
    const KIND: &'static str = "reward-intent-renewal";
    const VERSION: u16 = 1;
    const SIGNER: Option<KeyRole> = Some(KeyRole::Authorization);
    fn validate(&self) -> Result<()> {
        if self.renew_by_epoch <= self.issued_epoch {
            return Err(Error::Invalid("authorization renew-by hint"));
        }
        Ok(())
    }
}

/// The calculation target a validator holds active, rebuilt from its checked
/// history: the launch burn row while no cohort target has ever activated, or
/// the target of the latest checked closure that produced one, with that
/// closure's closing epoch. It is built only from those two sources. A
/// validator passes the one it holds at each tempo to
/// [`CheckedAuthorization::operation`], which re-checks it every time.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActiveTarget {
    chain: ChainBinding,
    digest: Digest,
    /// `None` for the launch burn row; otherwise the closing epoch of the
    /// checked closure that activated the target.
    activated_epoch: Option<u64>,
}

impl ActiveTarget {
    /// The launch burn row on `chain`, active until some cohort first
    /// produces a target.
    pub fn launch(chain: &ChainBinding) -> Result<Self> {
        Ok(Self {
            chain: chain.clone(),
            digest: CalculationTargetV2::launch_burn(chain.clone()).digest()?,
            activated_epoch: None,
        })
    }

    /// The target that `closure` activated at its closing epoch. A closure of
    /// a cohort that produced no target activates nothing.
    pub fn activated(closure: &CheckedClosure<'_>) -> Result<Self> {
        let closure = closure.closure();
        Ok(Self {
            chain: closure.chain.clone(),
            digest: closure
                .target
                .ok_or(Error::Invalid("closure activated no target"))?,
            activated_epoch: Some(closure.closed_epoch),
        })
    }

    /// Restores the active target from the holder's own durable record of
    /// one it built earlier with [`Self::launch`] or [`Self::activated`]
    /// (`activated_epoch` `None` only for the launch burn row of `chain`).
    /// A holder writes that record only from those two sources, so it is as
    /// trusted as the holder's journal; it is never read from a bundle.
    pub fn restore(chain: &ChainBinding, digest: Digest, activated_epoch: Option<u64>) -> Result<Self> {
        if activated_epoch.is_none() && digest != CalculationTargetV2::launch_burn(chain.clone()).digest()? {
            return Err(Error::Invalid("held launch target"));
        }
        Ok(Self {
            chain: chain.clone(),
            digest,
            activated_epoch,
        })
    }

    pub fn chain(&self) -> &ChainBinding {
        &self.chain
    }

    pub fn digest(&self) -> Digest {
        self.digest
    }

    /// Whether no cohort target has ever activated.
    pub fn is_launch_burn(&self) -> bool {
        self.activated_epoch.is_none()
    }

    /// The closing epoch of the closure that activated the target; `None` for
    /// the launch burn row.
    pub fn activated_epoch(&self) -> Option<u64> {
        self.activated_epoch
    }
}

impl LaunchAuthorization {
    /// The launch authorization on `chain` issued at `issued_epoch`, unsigned.
    pub fn new(
        chain: &ChainBinding,
        checkpoint_generation: u64,
        issued_epoch: u64,
    ) -> Result<Self> {
        Ok(Self {
            chain: chain.clone(),
            target: CalculationTargetV2::launch_burn(chain.clone()).digest()?,
            checkpoint_generation,
            issued_epoch,
            renew_by_epoch: issued_epoch
                .checked_add(LAUNCH_RENEW_BY_TEMPOS)
                .ok_or(Error::Overflow("authorization renew-by hint"))?,
        })
    }

    /// Checks a signed launch authorization for a validator on `chain` whose
    /// active target is `active`:
    ///
    /// - the signature verifies under the trusted authorization key;
    /// - it names `chain` and the launch burn row of `chain`;
    /// - its renew-by hint is [`LAUNCH_RENEW_BY_TEMPOS`] after its issue (a
    ///   format check; the hint never expires anything);
    /// - the active target is the launch burn row, so no cohort target has
    ///   ever activated.
    ///
    /// It stays valid after the first package is signed, while that
    /// package's target is pending, so validators keep committing the burn row
    /// through the first cohort's transition. [`CheckedAuthorization::operation`]
    /// refuses it again at every tempo once a target has activated.
    pub fn check<'a>(
        signed: &'a Signed<Self>,
        keys: &TrustedKeys,
        chain: &ChainBinding,
        active: &ActiveTarget,
    ) -> Result<CheckedAuthorization<'a>> {
        let authorization = signed.verify(keys)?;
        authorization.validate()?;
        if authorization.chain != *chain || active.chain != *chain {
            return Err(Error::Mismatch("authorization chain"));
        }
        let target = CalculationTargetV2::launch_burn(chain.clone());
        let digest = target.digest()?;
        if authorization.target != digest {
            return Err(Error::Mismatch("authorized target"));
        }
        if authorization.renew_by_epoch - authorization.issued_epoch != LAUNCH_RENEW_BY_TEMPOS {
            return Err(Error::Mismatch("authorization renew-by hint"));
        }
        if !active.is_launch_burn() || active.digest != digest {
            return Err(Error::Invalid("launch authorization after activation"));
        }
        Ok(CheckedAuthorization {
            digest: authorization.digest()?,
            scope: Scope::Launch {
                authorization,
                target,
            },
        })
    }
}

impl RewardIntentAuthorization {
    /// Distance of the non-binding renew-by hint from the issue, in tempos.
    pub fn renew_by_tempos(&self) -> u64 {
        self.renew_by_epoch.saturating_sub(self.issued_epoch)
    }

    /// Checks a signed authorization against the manifest of the cohort it
    /// was issued for, that cohort's package, the target it names, and the
    /// validator's active target:
    ///
    /// - the signature verifies under the trusted authorization key;
    /// - the chain, cohort, manifest and package digests are the cohort's;
    /// - the renew-by hint is the manifest's frozen `lease_tempos` after the
    ///   issue (a format check; the hint never expires anything);
    /// - the issue epoch is at or after the cohort's opening epoch;
    /// - the target is the named one, on the manifest's chain;
    /// - a package that produced a target authorizes exactly that target,
    ///   whose predecessor is the active target (the first cohort target's
    ///   predecessor is the launch burn row), or which is itself the active
    ///   target once its closure has activated it;
    /// - a package that produced none authorizes the active target, which is
    ///   the launch burn row only while no cohort target has ever activated,
    ///   and never a target of its own or a later cohort.
    ///
    /// A validator with a gap in its history holds a stale active target, so
    /// the check fails and new commitments pause until it fetches the missing
    /// package and closure. Returns the [`CheckedAuthorization`], the only
    /// form in which an authorization checks a renewal or yields a per-tempo
    /// commitment ([`CheckedAuthorization::operation`]).
    pub fn check<'a>(
        signed: &'a Signed<Self>,
        keys: &TrustedKeys,
        manifest: &'a CohortManifestV2,
        package: &'a ScorePackageV2,
        target: &'a CalculationTargetV2,
        active: &ActiveTarget,
    ) -> Result<CheckedAuthorization<'a>> {
        let authorization = signed.verify(keys)?;
        authorization.validate()?;
        target.validate()?;
        if authorization.chain != manifest.chain
            || authorization.cohort != manifest.cohort
            || authorization.cohort_manifest != manifest.digest()?
        {
            return Err(Error::Mismatch("authorization manifest"));
        }
        if authorization.renew_by_tempos() != u64::from(manifest.schedule.lease_tempos) {
            return Err(Error::Mismatch("authorization renew-by hint"));
        }
        if authorization.issued_epoch < manifest.opened_epoch {
            return Err(Error::Invalid("authorization before its cohort"));
        }
        let digest = target.digest()?;
        if authorization.target != digest || target.chain != manifest.chain {
            return Err(Error::Mismatch("authorized target"));
        }
        package.check_manifest(manifest)?;
        if authorization.score_package != package.digest()? {
            return Err(Error::Mismatch("authorization package"));
        }
        if active.chain != manifest.chain {
            return Err(Error::Mismatch("authorization chain"));
        }
        match package.mode {
            CutoffMode::Margin | CutoffMode::Fallback => {
                target.check_package(package)?;
                if digest != active.digest && target.predecessor != Some(active.digest) {
                    return Err(Error::Mismatch("target predecessor not active"));
                }
            }
            CutoffMode::NoTarget => {
                if let TargetSource::Cohort { cohort, .. } = &target.source
                    && *cohort >= package.cohort
                {
                    return Err(Error::Mismatch("carried-over target"));
                }
                if digest != active.digest {
                    return Err(Error::Mismatch("carried target not active"));
                }
            }
        }
        Ok(CheckedAuthorization {
            digest: authorization.digest()?,
            scope: Scope::Cohort {
                authorization,
                manifest,
                package,
                target,
            },
        })
    }
}

/// A short-lived borrow of what an authorization was checked against. The
/// launch variant holds its burn target by value (it is derived, not
/// borrowed); the size difference costs one stack value per check.
#[derive(Clone, Debug)]
#[allow(clippy::large_enum_variant)]
enum Scope<'a> {
    Cohort {
        authorization: &'a RewardIntentAuthorization,
        manifest: &'a CohortManifestV2,
        package: &'a ScorePackageV2,
        target: &'a CalculationTargetV2,
    },
    Launch {
        authorization: &'a LaunchAuthorization,
        target: CalculationTargetV2,
    },
}

/// A signed authorization that [`RewardIntentAuthorization::check`] or
/// [`LaunchAuthorization::check`] accepted: its signature verified under the
/// trusted authorization key, and it is bound to its carrying cohort's frozen
/// renew-by hint, opening, package and target and to the validator's active
/// target, or, for the launch authorization, to the chain, the launch burn
/// row, the fixed hint distance and an active target that is still the burn
/// row. It borrows
/// what it was checked against, so none changes while it exists; a validator
/// that journals an authorization checks it again after reloading it, and
/// every [`Self::operation`] re-checks the validator's current active target.
///
/// It is the only form in which an authorization checks a renewal
/// ([`RewardIntentRenewal::check`]) or yields a per-tempo commitment
/// ([`Self::operation`]). Only the two `check` functions build one, and an
/// unchecked authorization has no `operation`. This compiles:
///
/// ```
/// use slop_ninja_protocol::{
///     AccountId, ActiveTarget, ChainBinding, CheckedAuthorization, LaunchAuthorization, Result,
///     Signed, TrustedKeys, WeightOperationV2,
/// };
/// fn checked<'a>(
///     signed: &'a Signed<LaunchAuthorization>,
///     keys: &TrustedKeys,
///     chain: &ChainBinding,
/// ) -> Result<CheckedAuthorization<'a>> {
///     LaunchAuthorization::check(signed, keys, chain, &ActiveTarget::launch(chain)?)
/// }
/// fn commit(
///     authorization: &CheckedAuthorization<'_>,
///     active: &ActiveTarget,
///     hotkey: AccountId,
/// ) -> Result<WeightOperationV2> {
///     authorization.operation(None, &[], active, hotkey, 0)
/// }
/// ```
///
/// An unchecked authorization yields no operation:
///
/// ```compile_fail,E0599
/// use slop_ninja_protocol::{
///     AccountId, ActiveTarget, Result, RewardIntentAuthorization, WeightOperationV2,
/// };
/// fn commit(
///     authorization: &RewardIntentAuthorization,
///     active: &ActiveTarget,
///     hotkey: AccountId,
/// ) -> Result<WeightOperationV2> {
///     authorization.operation(None, &[], active, hotkey, 0)
/// }
/// ```
///
/// and a checked one cannot be built around it without `check`:
///
/// ```compile_fail,E0451
/// use slop_ninja_protocol::{CheckedAuthorization, Digest};
/// fn checked<'a>() -> CheckedAuthorization<'a> {
///     CheckedAuthorization { digest: Digest([0; 32]), scope: todo!() }
/// }
/// ```
#[derive(Clone, Debug)]
pub struct CheckedAuthorization<'a> {
    digest: Digest,
    scope: Scope<'a>,
}

impl<'a> CheckedAuthorization<'a> {
    /// Digest of the checked authorization, which a renewal names.
    pub fn digest(&self) -> Digest {
        self.digest
    }

    /// The checked per-cohort authorization; `None` for the launch one.
    pub fn cohort_authorization(&self) -> Option<&'a RewardIntentAuthorization> {
        match &self.scope {
            Scope::Cohort { authorization, .. } => Some(authorization),
            Scope::Launch { .. } => None,
        }
    }

    /// The checked launch authorization; `None` for a per-cohort one.
    pub fn launch_authorization(&self) -> Option<&'a LaunchAuthorization> {
        match &self.scope {
            Scope::Launch { authorization, .. } => Some(authorization),
            Scope::Cohort { .. } => None,
        }
    }

    /// The manifest of the cohort it was issued for; `None` for the launch one.
    pub fn manifest(&self) -> Option<&'a CohortManifestV2> {
        match &self.scope {
            Scope::Cohort { manifest, .. } => Some(manifest),
            Scope::Launch { .. } => None,
        }
    }

    /// That cohort's package; `None` for the launch authorization.
    pub fn package(&self) -> Option<&'a ScorePackageV2> {
        match &self.scope {
            Scope::Cohort { package, .. } => Some(package),
            Scope::Launch { .. } => None,
        }
    }

    /// The target it authorizes.
    pub fn target(&self) -> &CalculationTargetV2 {
        match &self.scope {
            Scope::Cohort { target, .. } => target,
            Scope::Launch { target, .. } => target,
        }
    }

    /// `(checkpoint_generation, issued_epoch, renew_by_epoch)`.
    fn terms(&self) -> (u64, u64, u64) {
        match &self.scope {
            Scope::Cohort { authorization, .. } => (
                authorization.checkpoint_generation,
                authorization.issued_epoch,
                authorization.renew_by_epoch,
            ),
            Scope::Launch { authorization, .. } => (
                authorization.checkpoint_generation,
                authorization.issued_epoch,
                authorization.renew_by_epoch,
            ),
        }
    }

    /// Generation of the coordinator checkpoint that issued it.
    pub fn checkpoint_generation(&self) -> u64 {
        self.terms().0
    }

    /// First epoch index at which the authorization yields a commitment.
    pub fn issued_epoch(&self) -> u64 {
        self.terms().1
    }

    /// The authorization's non-binding renew-by hint. Nothing refuses or
    /// pauses when it passes.
    pub fn renew_by_epoch(&self) -> u64 {
        self.terms().2
    }

    /// The renew-by distance a hint refresh keeps: the manifest's frozen
    /// `lease_tempos`, or [`LAUNCH_RENEW_BY_TEMPOS`].
    pub fn renew_by_tempos(&self) -> u64 {
        match &self.scope {
            Scope::Cohort { manifest, .. } => u64::from(manifest.schedule.lease_tempos),
            Scope::Launch { .. } => LAUNCH_RENEW_BY_TEMPOS,
        }
    }

    /// The chain the authorized target is on.
    pub fn chain(&self) -> &ChainBinding {
        &self.target().chain
    }

    fn check_renewal_binding(&self, renewal: &CheckedRenewal<'_>) -> Result<()> {
        if renewal.authorization != self.digest {
            return Err(Error::Mismatch("renewed authorization"));
        }
        Ok(())
    }

    /// The first of `revocations` that withdraws this authorization, if any
    /// ([`CheckedRevocation::withdraws`]).
    pub fn revoked_by<'r, 'b>(&self, revocations: &'r [CheckedRevocation<'b>]) -> Option<&'r CheckedRevocation<'b>> {
        revocations.iter().find(|r| r.withdraws(self))
    }

    /// Authorizes one new per-tempo commitment of the authorized target by
    /// validator `hotkey` at `epoch`, read from a fresh finalized view of the
    /// chain's epoch index. `active` is the target the validator holds active
    /// at that tempo, rebuilt from its checked history ([`ActiveTarget`]), and
    /// `revocations` every revocation it has checked and journaled; both are
    /// re-checked on every call:
    ///
    /// - the launch authorization is refused once any cohort target has
    ///   activated (`launch authorization after activation`), so one kept
    ///   across the first closing boundary commits the burn row no longer;
    /// - the authorized target must be `active` on its chain (`authorized
    ///   target not active`), so a cohort authorization stops once a later
    ///   cohort's target activates (supersession), and a pending target is
    ///   never committed before its closure;
    /// - `epoch` must be at or after the target's activation, the closing
    ///   epoch of the checked closure that activated it
    ///   ([`crate::submission::CohortClosure::check`], whose `closed_epoch`
    ///   already satisfies the manifest's admission, transition and cohort
    ///   minimums); the launch burn row has none;
    /// - `epoch` must be at or after the authorization's `issued_epoch`;
    /// - no revocation may withdraw it (`revoked`).
    ///
    /// There is no end epoch: neither the renew-by hint nor any renewal is
    /// consulted, so a commitment at any later epoch is authorized while the
    /// target stays active and unrevoked. A `renewal`, when given, must name
    /// this authorization and changes nothing else.
    pub fn operation(
        &self,
        renewal: Option<&CheckedRenewal<'_>>,
        revocations: &[CheckedRevocation<'_>],
        active: &ActiveTarget,
        hotkey: AccountId,
        epoch: u64,
    ) -> Result<WeightOperationV2> {
        if let Some(renewal) = renewal {
            self.check_renewal_binding(renewal)?;
        }
        let target = self.target();
        if matches!(self.scope, Scope::Launch { .. }) && !active.is_launch_burn() {
            return Err(Error::Invalid("launch authorization after activation"));
        }
        if active.chain != target.chain || active.digest != target.digest()? {
            return Err(Error::Mismatch("authorized target not active"));
        }
        if active
            .activated_epoch
            .is_some_and(|activated| epoch < activated)
        {
            return Err(Error::Invalid("epoch before target activation"));
        }
        if epoch < self.issued_epoch() {
            return Err(Error::Invalid("epoch before authorization"));
        }
        if revocations.iter().any(|r| r.withdraws(self)) {
            return Err(Error::Revoked("reward intent authorization"));
        }
        WeightOperationV2::for_target(target, hotkey, epoch)
    }
}

/// A signed [`RewardIntentRenewal`] that [`RewardIntentRenewal::check`]
/// accepted for one [`CheckedAuthorization`]: its signature verified under
/// the trusted authorization key, and it refreshes that authorization's
/// renew-by hint with the same distance. It authorizes nothing by itself.
#[derive(Clone, Copy, Debug)]
pub struct CheckedRenewal<'a> {
    renewal: &'a RewardIntentRenewal,
    authorization: Digest,
}

impl<'a> CheckedRenewal<'a> {
    /// The checked renewal.
    pub fn renewal(&self) -> &'a RewardIntentRenewal {
        self.renewal
    }
}

impl RewardIntentRenewal {
    /// The hint refresh of the checked `authorization` issued at
    /// `issued_epoch`, unsigned.
    pub fn new(
        authorization: &CheckedAuthorization<'_>,
        checkpoint_generation: u64,
        issued_epoch: u64,
    ) -> Result<Self> {
        let renewal = Self {
            authorization: authorization.digest,
            checkpoint_generation,
            issued_epoch,
            renew_by_epoch: issued_epoch
                .checked_add(authorization.renew_by_tempos())
                .ok_or(Error::Overflow("authorization renew-by hint"))?,
        };
        renewal.check_record(authorization)?;
        Ok(renewal)
    }

    /// Checks a signed hint refresh of the checked `authorization`: the
    /// signature verifies under the trusted authorization key, it names that
    /// authorization, keeps its renew-by distance, and is never of an earlier
    /// issue or checkpoint generation.
    pub fn check<'a>(
        signed: &'a Signed<Self>,
        keys: &TrustedKeys,
        authorization: &CheckedAuthorization<'_>,
    ) -> Result<CheckedRenewal<'a>> {
        let renewal = signed.verify(keys)?;
        renewal.check_record(authorization)?;
        Ok(CheckedRenewal {
            renewal,
            authorization: authorization.digest,
        })
    }

    fn check_record(&self, authorization: &CheckedAuthorization<'_>) -> Result<()> {
        self.validate()?;
        if self.authorization != authorization.digest {
            return Err(Error::Mismatch("renewed authorization"));
        }
        let (generation, issued, _) = authorization.terms();
        if self.checkpoint_generation < generation {
            return Err(Error::Invalid("renewal checkpoint generation"));
        }
        if self.issued_epoch < issued {
            return Err(Error::Invalid("renewal before its authorization"));
        }
        if self.renew_by_epoch - self.issued_epoch != authorization.renew_by_tempos() {
            return Err(Error::Mismatch("renewal renew-by hint"));
        }
        Ok(())
    }
}

canonical_struct! {
    /// The signed withdrawal of a reward-intent or launch authorization's
    /// target: every authorization of `target` on `chain` issued at or before
    /// `issued_epoch` stops yielding new commitments for every validator that
    /// holds this revocation. An authorization of the same target issued at a
    /// later epoch re-authorizes it. Commitments already submitted are left to
    /// the chain; validators retire their queued, never-signed work of the
    /// target.
    pub struct RewardIntentRevocation {
        pub chain: ChainBinding,
        /// Digest of the withdrawn calculation target.
        pub target: Digest,
        /// Digest of the authorization the revoker issued last for `target`
        /// (an audit binding; the withdrawal covers every authorization of
        /// `target` issued at or before `issued_epoch`).
        pub authorization: Digest,
        /// Never below that authorization's.
        pub checkpoint_generation: u64,
        /// Never before that authorization's issue.
        pub issued_epoch: u64,
    }
}

impl Record for RewardIntentRevocation {
    const KIND: &'static str = "reward-intent-revocation";
    const VERSION: u16 = 1;
    const SIGNER: Option<KeyRole> = Some(KeyRole::Authorization);
    fn validate(&self) -> Result<()> {
        Ok(())
    }
}

/// A signed [`RewardIntentRevocation`] that [`RewardIntentRevocation::check`]
/// accepted (its signature verified under the trusted authorization key and
/// it names the validator's chain), or one its holder journaled after that
/// check ([`RewardIntentRevocation::journaled`]). It is the only form a
/// revocation takes in [`CheckedAuthorization::operation`].
#[derive(Clone, Copy, Debug)]
pub struct CheckedRevocation<'a> {
    revocation: &'a RewardIntentRevocation,
    digest: Digest,
}

impl<'a> CheckedRevocation<'a> {
    /// The checked revocation.
    pub fn revocation(&self) -> &'a RewardIntentRevocation {
        self.revocation
    }

    /// Digest of the revocation record.
    pub fn digest(&self) -> Digest {
        self.digest
    }

    /// Whether it withdraws `authorization`: the same chain and target, and
    /// the authorization issued at or before the revocation.
    pub fn withdraws(&self, authorization: &CheckedAuthorization<'_>) -> bool {
        let r = self.revocation;
        authorization.chain() == &r.chain
            && authorization.target().digest().is_ok_and(|d| d == r.target)
            && authorization.issued_epoch() <= r.issued_epoch
    }
}

impl RewardIntentRevocation {
    /// The revocation of the checked `authorization`'s target issued at
    /// `issued_epoch`, unsigned.
    pub fn new(
        authorization: &CheckedAuthorization<'_>,
        checkpoint_generation: u64,
        issued_epoch: u64,
    ) -> Result<Self> {
        let (generation, issued, _) = authorization.terms();
        if checkpoint_generation < generation {
            return Err(Error::Invalid("revocation checkpoint generation"));
        }
        if issued_epoch < issued {
            return Err(Error::Invalid("revocation before its authorization"));
        }
        Ok(Self {
            chain: authorization.chain().clone(),
            target: authorization.target().digest()?,
            authorization: authorization.digest,
            checkpoint_generation,
            issued_epoch,
        })
    }

    /// A revocation its holder checked ([`Self::check`]) when it journaled
    /// it, as every later check takes it. Its signature is not verified
    /// again: a revocation can only withdraw, never authorize, so trusting
    /// the holder's own journal can never yield a commitment, and a trust
    /// root rotated since it was journaled must not turn it into an error
    /// that stops commitments of other targets. It withdraws nothing on
    /// another chain or of another target ([`CheckedRevocation::withdraws`]).
    pub fn journaled(&self) -> Result<CheckedRevocation<'_>> {
        Ok(CheckedRevocation {
            revocation: self,
            digest: self.digest()?,
        })
    }

    /// Checks a signed revocation for a validator on `chain`: the signature
    /// verifies under the trusted authorization key and it names `chain`.
    pub fn check<'a>(signed: &'a Signed<Self>, keys: &TrustedKeys, chain: &ChainBinding) -> Result<CheckedRevocation<'a>> {
        let revocation = signed.verify(keys)?;
        revocation.validate()?;
        if revocation.chain != *chain {
            return Err(Error::Mismatch("revocation chain"));
        }
        Ok(CheckedRevocation {
            revocation,
            digest: revocation.digest()?,
        })
    }
}
