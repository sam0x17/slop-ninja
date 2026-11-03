//! The validators' reward-intent authorizations and their per-tempo renewal.
//!
//! Two records authorize new weight commitments, both signed under
//! [`KeyRole::Authorization`]:
//!
//! - A [`LaunchAuthorization`] names only the chain and the launch burn row,
//!   with the protocol-fixed lease [`LAUNCH_LEASE_TEMPOS`] and no manifest, so
//!   validators commit the burn row from the first tempo of the first cohort,
//!   before any manifest is sealed. It serves until the first cohort target
//!   activates at its closing boundary. Between the first package's
//!   publication and that boundary the new target is pending and the burn row
//!   is still the active target, so the launch authorization keeps validators
//!   committing without a gap, and it can only ever commit the burn row, which
//!   pays no miner. From that boundary every validator refuses it, at its
//!   check and at every per-tempo operation, so the burn row is never
//!   re-authorized after a cohort target has been active.
//! - Each score package is published with a per-cohort
//!   [`RewardIntentAuthorization`] for the target active after it: the
//!   package's new target, whose predecessor must be the validator's active
//!   target, or, when the cohort produced none, the target already active. Its
//!   lease is the manifest's frozen `lease_tempos`, at least two cohort
//!   lengths.
//!
//! A lease runs from `issued_epoch` through `expires_epoch` inclusive. While
//! the coordinator is live it issues a [`RewardIntentRenewal`] each tempo. A
//! renewal names the authorization it renews, so it cannot change the target,
//! keeps the lease length and takes effect only from its own issue epoch.
//!
//! Checking takes the signed envelope, never the bare record:
//! [`LaunchAuthorization::check`] and [`RewardIntentAuthorization::check`]
//! verify the signature under the trusted authorization key and return a
//! [`CheckedAuthorization`], and [`RewardIntentRenewal::check`] verifies a
//! renewal's signature against that and returns a [`CheckedRenewal`]. Only a
//! checked authorization, with at most one checked renewal of it, yields a new
//! per-tempo commitment ([`CheckedAuthorization::operation`]), with the epoch
//! index read from a fresh finalized view of the chain and the validator's
//! [`ActiveTarget`] as of that tempo. The operation re-checks the active
//! target every time: the authorized target must be the active one, and the
//! epoch at or after its activation (the closing epoch of its
//! [`CheckedClosure`]; the launch burn row has none) and inside the
//! authorization's lease or the renewal's. An authorization checked earlier
//! therefore stops yielding commitments as soon as another target activates.
//! Beyond both leases it pauses new commitments. Commitments already
//! submitted keep their recorded authority and native deadlines; reconciling
//! them recomputes their identity with [`WeightOperationV2::for_target`] and
//! needs no lease.
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

/// Lease of a [`LaunchAuthorization`] and of each of its renewals: 40 tempos,
/// two launch cohorts. No manifest exists yet to freeze one, and the row it
/// authorizes is the burn row, which pays no miner.
pub const LAUNCH_LEASE_TEMPOS: u64 = 40;

canonical_struct! {
    /// The manifest-free authorization of the launch burn row, valid while no
    /// cohort target has activated: through the first package's publication
    /// and until that package's target activates at its closing boundary.
    pub struct LaunchAuthorization {
        pub chain: ChainBinding,
        /// Digest of [`CalculationTargetV2::launch_burn`] on `chain`.
        pub target: Digest,
        /// Generation of the coordinator checkpoint that issued it.
        pub checkpoint_generation: u64,
        /// First epoch index of the lease.
        pub issued_epoch: u64,
        /// `issued_epoch + LAUNCH_LEASE_TEMPOS`.
        pub expires_epoch: u64,
    }
}

impl Record for LaunchAuthorization {
    const KIND: &'static str = "launch-authorization";
    const VERSION: u16 = 1;
    const SIGNER: Option<KeyRole> = Some(KeyRole::Authorization);
    fn validate(&self) -> Result<()> {
        if self.expires_epoch <= self.issued_epoch {
            return Err(Error::Invalid("authorization lease"));
        }
        Ok(())
    }
}

canonical_struct! {
    /// Per-cohort reward-intent authorization for the target active after a
    /// score package.
    pub struct RewardIntentAuthorization {
        pub chain: ChainBinding,
        /// The cohort whose package carries it.
        pub cohort: u64,
        pub cohort_manifest: Digest,
        /// The package that carries it.
        pub score_package: Digest,
        /// Digest of the target active after the package: new or carried over.
        pub target: Digest,
        /// Generation of the coordinator checkpoint that issued it.
        pub checkpoint_generation: u64,
        /// First epoch index of the lease.
        pub issued_epoch: u64,
        /// Last epoch index at which a new commitment may be made:
        /// `issued_epoch` plus the manifest's `lease_tempos`.
        pub expires_epoch: u64,
    }
}

impl Record for RewardIntentAuthorization {
    const KIND: &'static str = "reward-intent-authorization";
    const VERSION: u16 = 2;
    const SIGNER: Option<KeyRole> = Some(KeyRole::Authorization);
    fn validate(&self) -> Result<()> {
        if self.expires_epoch <= self.issued_epoch {
            return Err(Error::Invalid("authorization lease"));
        }
        Ok(())
    }
}

canonical_struct! {
    /// The coordinator's per-tempo renewal of a reward-intent or launch
    /// authorization. It leaves the target unchanged and takes effect only
    /// from its own issue epoch.
    pub struct RewardIntentRenewal {
        /// Digest of the authorization it renews.
        pub authorization: Digest,
        /// Never below the authorization's.
        pub checkpoint_generation: u64,
        /// Never before the authorization's.
        pub issued_epoch: u64,
        /// `issued_epoch` plus the authorization's lease length.
        pub expires_epoch: u64,
    }
}

impl Record for RewardIntentRenewal {
    const KIND: &'static str = "reward-intent-renewal";
    const VERSION: u16 = 1;
    const SIGNER: Option<KeyRole> = Some(KeyRole::Authorization);
    fn validate(&self) -> Result<()> {
        if self.expires_epoch <= self.issued_epoch {
            return Err(Error::Invalid("authorization lease"));
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
            expires_epoch: issued_epoch
                .checked_add(LAUNCH_LEASE_TEMPOS)
                .ok_or(Error::Overflow("authorization lease"))?,
        })
    }

    /// Checks a signed launch authorization for a validator on `chain` whose
    /// active target is `active`:
    ///
    /// - the signature verifies under the trusted authorization key;
    /// - it names `chain` and the launch burn row of `chain`;
    /// - its lease is [`LAUNCH_LEASE_TEMPOS`];
    /// - the active target is the launch burn row, so no cohort target has
    ///   ever activated.
    ///
    /// It stays valid after the first package is published, while that
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
        if authorization.expires_epoch - authorization.issued_epoch != LAUNCH_LEASE_TEMPOS {
            return Err(Error::Mismatch("authorization lease"));
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
    /// Lease length in tempos.
    pub fn lease_tempos(&self) -> u64 {
        self.expires_epoch.saturating_sub(self.issued_epoch)
    }

    /// Checks a signed authorization against the manifest of the cohort that
    /// carries it, that cohort's package, the target it names, and the
    /// validator's active target:
    ///
    /// - the signature verifies under the trusted authorization key;
    /// - the chain, cohort, manifest and package digests are the cohort's;
    /// - the lease is the manifest's frozen `lease_tempos`;
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
        if authorization.lease_tempos() != u64::from(manifest.schedule.lease_tempos) {
            return Err(Error::Mismatch("authorization lease"));
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

#[derive(Clone, Debug)]
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
/// lease, opening, package and target and to the validator's active target,
/// or, for the launch authorization, to the chain, the launch burn row, the
/// fixed lease and an active target that is still the burn row. It borrows
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
///     authorization.operation(None, active, hotkey, 0)
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
///     authorization.operation(None, active, hotkey, 0)
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

    /// The manifest of the cohort that carries it; `None` for the launch one.
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

    /// `(checkpoint_generation, issued_epoch, expires_epoch)`.
    fn lease(&self) -> (u64, u64, u64) {
        match &self.scope {
            Scope::Cohort { authorization, .. } => (
                authorization.checkpoint_generation,
                authorization.issued_epoch,
                authorization.expires_epoch,
            ),
            Scope::Launch { authorization, .. } => (
                authorization.checkpoint_generation,
                authorization.issued_epoch,
                authorization.expires_epoch,
            ),
        }
    }

    /// First epoch index of the authorization's own lease.
    pub fn issued_epoch(&self) -> u64 {
        self.lease().1
    }

    /// Last epoch index of the authorization's own lease.
    pub fn expires_epoch(&self) -> u64 {
        self.lease().2
    }

    /// The lease length every renewal keeps: the manifest's frozen
    /// `lease_tempos`, or [`LAUNCH_LEASE_TEMPOS`].
    pub fn lease_tempos(&self) -> u64 {
        match &self.scope {
            Scope::Cohort { manifest, .. } => u64::from(manifest.schedule.lease_tempos),
            Scope::Launch { .. } => LAUNCH_LEASE_TEMPOS,
        }
    }

    /// Whether a new commitment at `epoch` lies inside the authorization's
    /// own lease or, when given, the checked renewal's. A renewal takes effect
    /// only from its issue epoch, so a lapsed lease resumes there and a
    /// pre-dated renewal extends nothing ahead of time.
    pub fn covers(&self, renewal: Option<&CheckedRenewal<'_>>, epoch: u64) -> Result<bool> {
        let (_, issued, expires) = self.lease();
        let own = (issued..=expires).contains(&epoch);
        let renewed = match renewal {
            None => false,
            Some(r) => {
                self.check_renewal_binding(r)?;
                (r.renewal.issued_epoch..=r.renewal.expires_epoch).contains(&epoch)
            }
        };
        Ok(own || renewed)
    }

    fn check_renewal_binding(&self, renewal: &CheckedRenewal<'_>) -> Result<()> {
        if renewal.authorization != self.digest {
            return Err(Error::Mismatch("renewed authorization"));
        }
        Ok(())
    }

    /// Authorizes one new per-tempo commitment of the authorized target by
    /// validator `hotkey` at `epoch`, read from a fresh finalized view of the
    /// chain's epoch index. `active` is the target the validator holds active
    /// at that tempo, rebuilt from its checked history ([`ActiveTarget`]), and
    /// it is re-checked on every call:
    ///
    /// - the launch authorization is refused once any cohort target has
    ///   activated (`launch authorization after activation`), so one kept
    ///   across the first closing boundary commits the burn row no longer;
    /// - the authorized target must be `active` on its chain (`authorized
    ///   target not active`), so a cohort authorization stops once a later
    ///   cohort's target activates, and a pending target is never committed
    ///   before its closure;
    /// - `epoch` must be at or after the target's activation, the closing
    ///   epoch of the checked closure that activated it
    ///   ([`crate::submission::CohortClosure::check`], whose `closed_epoch`
    ///   already satisfies the manifest's admission, transition and cohort
    ///   minimums); the launch burn row has none;
    /// - `epoch` must lie inside the authorization's own lease or the checked
    ///   `renewal`'s ([`Self::covers`]).
    pub fn operation(
        &self,
        renewal: Option<&CheckedRenewal<'_>>,
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
        if !self.covers(renewal, epoch)? {
            let (_, issued, expires) = self.lease();
            if epoch < issued {
                return Err(Error::Invalid("epoch before authorization"));
            }
            if epoch > expires && renewal.is_some_and(|r| epoch < r.renewal.issued_epoch) {
                return Err(Error::Invalid("epoch before renewal issue"));
            }
            return Err(Error::Expired("reward intent lease"));
        }
        WeightOperationV2::for_target(target, hotkey, epoch)
    }
}

/// A signed [`RewardIntentRenewal`] that [`RewardIntentRenewal::check`]
/// accepted for one [`CheckedAuthorization`]: its signature verified under
/// the trusted authorization key, and it renews that authorization with the
/// same lease length. It is the only form a renewal takes in
/// [`CheckedAuthorization::operation`].
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
    /// The renewal of the checked `authorization` issued at `issued_epoch`,
    /// unsigned.
    pub fn new(
        authorization: &CheckedAuthorization<'_>,
        checkpoint_generation: u64,
        issued_epoch: u64,
    ) -> Result<Self> {
        let renewal = Self {
            authorization: authorization.digest,
            checkpoint_generation,
            issued_epoch,
            expires_epoch: issued_epoch
                .checked_add(authorization.lease_tempos())
                .ok_or(Error::Overflow("authorization lease"))?,
        };
        renewal.check_record(authorization)?;
        Ok(renewal)
    }

    /// Checks a signed renewal of the checked `authorization`: the signature
    /// verifies under the trusted authorization key, it names that
    /// authorization, keeps its lease length, and is never of an earlier issue
    /// or checkpoint generation.
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
        let (generation, issued, _) = authorization.lease();
        if self.checkpoint_generation < generation {
            return Err(Error::Invalid("renewal checkpoint generation"));
        }
        if self.issued_epoch < issued {
            return Err(Error::Invalid("renewal before its authorization"));
        }
        if self.expires_epoch - self.issued_epoch != authorization.lease_tempos() {
            return Err(Error::Mismatch("renewal lease"));
        }
        Ok(())
    }
}
