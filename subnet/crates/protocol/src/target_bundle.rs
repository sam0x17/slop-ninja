//! Verification of a `reward-v1` active-target bundle: the signed records a
//! validator (the coordinator included) needs before it derives and commits
//! the active target's row at a tempo.
//!
//! A bundle is either the launch burn row, under a [`LaunchAuthorization`], or
//! a cohort target, with every record that activates and pays it:
//!
//! - the manifest, the score package, the calculation target and the
//!   cohort's signed closure, each authenticated before it is decoded;
//! - the published index as of the manifest (contents and fingerprint index),
//!   which must match the manifest's binding;
//! - the signed receipt index, bound to the package's root and count, and,
//!   where the holder retains them, the receipt chunk objects, each receipt
//!   checked against its index entry exactly once;
//! - the reward-intent authorization for the target, with an optional
//!   renewal, checked against its carrying cohort's manifest and package
//!   (the target's own cohort, or a later cohort that produced no target and
//!   carried it).
//!
//! [`verify`] recomputes the outcome ([`crate::reward_v1::check_outcome`]), so
//! every credit and every member's units are rebuilt and any difference is
//! refused before a row exists. A carrying cohort's package is verified the
//! same way: its outcome is recomputed against its own published index and
//! must be `NoTarget`, its receipt index is bound to its root and count, and
//! its published index must contain everything the target's cohort published
//! and every content that cohort closed with.
//!
//! The active target is never taken from the bundle. The holder passes the
//! target it holds active, from its own durable record of the checked
//! closures it accepted ([`ActiveTarget::restore`]): the launch burn row
//! until a cohort target has ever activated. A launch bundle is accepted only
//! while the holder still holds the launch burn row; a cohort bundle only when
//! its target is the held one (with the same activation epoch) or the held
//! target's successor, activated by the bundle's own checked closure. The
//! holder records [`VerifiedTarget::active`] after a successful verification,
//! so a superseded bundle, or the launch authorization after the first
//! activation, is refused from then on. [`VerifiedTarget::operation`]
//! re-checks the authorization at every tempo and
//! [`VerifiedTarget::derive_row`] derives the canonical row against the
//! holder's own finalized snapshot.
//!
//! The published index a bundle carries is the coordinator's: the holder
//! checks it against the signed manifest binding and that a carrier's index
//! extends it, but does not rebuild it from every closed cohort's packages.

use crate::authorization::{ActiveTarget, LaunchAuthorization, RewardIntentAuthorization, RewardIntentRenewal};
use crate::records_v2::{
    CalculationTargetV2, CohortManifestV2, FingerprintIndex, PublishedContents, PublishedIndex,
    RegistrationSnapshotV2, ScorePackageV2, WeightRowV2,
};
use crate::submission::CohortClosure;
use crate::{
    AccountId, AttemptReceipt, ChainBinding, Digest, Error, ReceiptIndex, Record, Result, Signed, TrustedKeys,
    WeightOperationV2,
};

/// The bytes of a cohort target's records.
#[derive(Clone, Copy, Debug)]
pub struct Source<'a> {
    pub manifest: &'a [u8],
    pub package: &'a [u8],
    pub target: &'a [u8],
    pub closure: &'a [u8],
    /// The signed receipt index. A holder that produced the receipts itself
    /// (the coordinator) may leave it out; every other validator includes it
    /// and verifies the retained receipts ([`VerifiedTarget::verify_receipts`]).
    pub receipt_index: Option<&'a [u8]>,
    pub published_contents: &'a [u8],
    pub fingerprint_index: &'a [u8],
}

/// A later cohort that produced no target and carries the active one: its
/// signed manifest and package, its signed receipt index (a holder that did
/// not produce the receipts includes it), and its published index as of its
/// manifest.
#[derive(Clone, Copy, Debug)]
pub struct Carrier<'a> {
    pub manifest: &'a [u8],
    pub package: &'a [u8],
    pub receipt_index: Option<&'a [u8]>,
    pub published_contents: &'a [u8],
    pub fingerprint_index: &'a [u8],
}

/// A carrying cohort's verified records.
#[derive(Clone, Debug)]
pub struct VerifiedCarrier {
    pub manifest: CohortManifestV2,
    pub package: ScorePackageV2,
    pub index: PublishedIndex,
    pub receipt_index: Option<ReceiptIndex>,
}

/// What authorizes the active target.
#[derive(Clone, Copy, Debug)]
pub enum Authorizing<'a> {
    Launch {
        authorization: &'a [u8],
        renewal: Option<&'a [u8]>,
    },
    Cohort {
        authorization: &'a [u8],
        renewal: Option<&'a [u8]>,
        /// `None`: the target's own cohort carries the authorization.
        carrier: Option<Carrier<'a>>,
    },
}

/// A cohort target's verified records.
#[derive(Clone, Debug)]
pub struct VerifiedSource {
    pub manifest: CohortManifestV2,
    pub package: ScorePackageV2,
    pub closure: CohortClosure,
    pub index: PublishedIndex,
    pub receipt_index: Option<ReceiptIndex>,
}

#[derive(Clone, Debug)]
enum Auth {
    Launch {
        authorization: Signed<LaunchAuthorization>,
        renewal: Option<Signed<RewardIntentRenewal>>,
    },
    Cohort {
        authorization: Signed<RewardIntentAuthorization>,
        renewal: Option<Signed<RewardIntentRenewal>>,
        carrier: Option<Box<VerifiedCarrier>>,
    },
}

/// An authenticated bundle whose records are mutually bound.
#[derive(Clone, Debug)]
pub struct VerifiedTarget {
    pub chain: ChainBinding,
    pub target: CalculationTargetV2,
    pub target_digest: Digest,
    /// The target active after this bundle: the held target, or its
    /// successor that the bundle's checked closure activated. The holder
    /// records it durably.
    pub active: ActiveTarget,
    pub source: Option<VerifiedSource>,
    auth: Auth,
}

/// Verifies receipt chunk objects (`u32_le(len) || receipt`, repeated) against
/// the index: every receipt matches its entry, and every entry is stored
/// exactly once. Returns the receipts.
pub fn verify_receipt_chunks(index: &ReceiptIndex, chunks: &[&[u8]]) -> Result<Vec<AttemptReceipt>> {
    let mut seen = vec![false; index.entries.len()];
    let mut out = Vec::with_capacity(index.entries.len());
    for chunk in chunks {
        let mut rest: &[u8] = chunk;
        while !rest.is_empty() {
            let (len, tail) = rest.split_first_chunk::<4>().ok_or(Error::Mismatch("receipt chunk length"))?;
            let len = u32::from_le_bytes(*len) as usize;
            if tail.len() < len {
                return Err(Error::Mismatch("receipt chunk truncated"));
            }
            let (bytes, tail) = tail.split_at(len);
            rest = tail;
            let receipt = AttemptReceipt::from_bytes(bytes)?;
            let position = index
                .entries
                .binary_search_by(|e| e.attempt.cmp(&receipt.attempt))
                .map_err(|_| Error::Mismatch("receipt not in index"))?;
            let slot = seen.get_mut(position).ok_or(Error::OutOfRange("receipt position"))?;
            if core::mem::replace(slot, true) {
                return Err(Error::Duplicate("receipt stored twice"));
            }
            out.push(index.verify_object(position, bytes)?);
        }
    }
    if seen.iter().any(|s| !s) {
        return Err(Error::Mismatch("index entry without a stored receipt"));
    }
    Ok(out)
}

fn verify_source(chain: &ChainBinding, keys: &TrustedKeys, s: &Source<'_>) -> Result<(VerifiedSource, CalculationTargetV2)> {
    let manifest = Signed::<CohortManifestV2>::open(s.manifest, keys)?;
    let package = Signed::<ScorePackageV2>::open(s.package, keys)?;
    let target = Signed::<CalculationTargetV2>::open(s.target, keys)?;
    let closure = Signed::<CohortClosure>::open(s.closure, keys)?;
    let receipt_index = s.receipt_index.map(|b| Signed::<ReceiptIndex>::open(b, keys)).transpose()?;
    if package.chain != *chain || target.chain != *chain || manifest.chain != *chain {
        return Err(Error::Mismatch("bundle chain"));
    }
    package.check_manifest(&manifest)?;
    target.check_package(&package)?;
    if let Some(index) = &receipt_index {
        package.check_receipts(index)?;
    }
    let index = PublishedIndex {
        contents: PublishedContents::from_bytes(s.published_contents)?,
        fingerprints: FingerprintIndex::from_bytes(s.fingerprint_index)?,
    };
    index.validate()?;
    if index.binding()? != manifest.published {
        return Err(Error::Mismatch("published index"));
    }
    crate::reward_v1::check_outcome(&package, &index)?;
    closure.check(&manifest, &package, Some(&target))?;
    Ok((
        VerifiedSource {
            manifest,
            package,
            closure,
            index,
            receipt_index,
        },
        target,
    ))
}

fn verify_carrier(
    chain: &ChainBinding,
    keys: &TrustedKeys,
    c: &Carrier<'_>,
    source: &VerifiedSource,
) -> Result<VerifiedCarrier> {
    let manifest = Signed::<CohortManifestV2>::open(c.manifest, keys)?;
    let package = Signed::<ScorePackageV2>::open(c.package, keys)?;
    let receipt_index = c.receipt_index.map(|b| Signed::<ReceiptIndex>::open(b, keys)).transpose()?;
    if manifest.chain != *chain || package.chain != *chain {
        return Err(Error::Mismatch("carrier chain"));
    }
    package.check_manifest(&manifest)?;
    if let Some(index) = &receipt_index {
        package.check_receipts(index)?;
    }
    if package.cohort <= source.package.cohort {
        return Err(Error::Invalid("carrier is not a later cohort"));
    }
    let index = PublishedIndex {
        contents: PublishedContents::from_bytes(c.published_contents)?,
        fingerprints: FingerprintIndex::from_bytes(c.fingerprint_index)?,
    };
    index.validate()?;
    if index.binding()? != manifest.published {
        return Err(Error::Mismatch("carrier published index"));
    }
    // Everything published before the target's cohort, and every content
    // that cohort closed with, is still published when the carrier sealed.
    let kept = source
        .index
        .contents
        .contents
        .iter()
        .chain(source.package.entrants.iter().map(|e| &e.content))
        .all(|content| index.contains(content));
    let fingerprints_kept = source
        .index
        .fingerprints
        .entries
        .iter()
        .all(|f| index.fingerprints.entries.binary_search(f).is_ok());
    if !kept || !fingerprints_kept {
        return Err(Error::Mismatch("carrier index drops published artifacts"));
    }
    // The carrier's outcome is recomputed like the target's own: a package
    // relabelled as producing no target is refused here.
    let evaluation = crate::reward_v1::check_outcome(&package, &index)?;
    if evaluation.mode != crate::records_v2::CutoffMode::NoTarget || package.mode != evaluation.mode {
        return Err(Error::Mismatch("carrier produced a target"));
    }
    Ok(VerifiedCarrier {
        manifest,
        package,
        index,
        receipt_index,
    })
}

/// The active target after a cohort bundle whose own checked closure
/// activates `target` with `activation`, for a holder that holds `held`.
fn next_active(held: &ActiveTarget, target: &CalculationTargetV2, activation: ActiveTarget) -> Result<ActiveTarget> {
    if activation.digest() == held.digest() {
        if activation.activated_epoch() != held.activated_epoch() {
            return Err(Error::Mismatch("held target activation epoch"));
        }
        return Ok(held.clone());
    }
    if target.predecessor != Some(held.digest()) {
        return Err(Error::Mismatch("bundle target is neither the held target nor its successor"));
    }
    if held
        .activated_epoch()
        .zip(activation.activated_epoch())
        .is_some_and(|(before, after)| after < before)
    {
        return Err(Error::Invalid("successor activates before the held target"));
    }
    Ok(activation)
}

/// Verifies a bundle for `chain` under the trusted application keys, for a
/// holder whose own checked history holds `held` active. Every refusal
/// happens before any row exists.
pub fn verify(
    chain: &ChainBinding,
    keys: &TrustedKeys,
    held: &ActiveTarget,
    source: Option<Source<'_>>,
    authorizing: Authorizing<'_>,
) -> Result<VerifiedTarget> {
    if held.chain() != chain {
        return Err(Error::Mismatch("held target chain"));
    }
    let (target, active, source) = match &source {
        None => {
            if !held.is_launch_burn() {
                return Err(Error::Invalid("launch authorization after activation"));
            }
            (CalculationTargetV2::launch_burn(chain.clone()), held.clone(), None)
        }
        Some(s) => {
            let (v, target) = verify_source(chain, keys, s)?;
            let activation = {
                let checked = v.closure.check(&v.manifest, &v.package, Some(&target))?;
                ActiveTarget::activated(&checked)?
            };
            let active = next_active(held, &target, activation)?;
            (target, active, Some(v))
        }
    };
    let decode_renewal = |r: Option<&[u8]>| r.map(Signed::<RewardIntentRenewal>::from_bytes).transpose();
    let auth = match authorizing {
        Authorizing::Launch { authorization, renewal } => {
            if source.is_some() {
                return Err(Error::Invalid("launch authorization of a cohort target"));
            }
            Auth::Launch {
                authorization: Signed::<LaunchAuthorization>::from_bytes(authorization)?,
                renewal: decode_renewal(renewal)?,
            }
        }
        Authorizing::Cohort {
            authorization,
            renewal,
            carrier,
        } => {
            let Some(verified_source) = source.as_ref() else {
                return Err(Error::Invalid("cohort authorization without its target"));
            };
            let carrier = match carrier {
                None => None,
                Some(c) => Some(Box::new(verify_carrier(chain, keys, &c, verified_source)?)),
            };
            Auth::Cohort {
                authorization: Signed::<RewardIntentAuthorization>::from_bytes(authorization)?,
                renewal: decode_renewal(renewal)?,
                carrier,
            }
        }
    };
    let verified = VerifiedTarget {
        chain: chain.clone(),
        target_digest: target.digest()?,
        target,
        active,
        source,
        auth,
    };
    // The authorization must check now; each operation checks it again.
    verified.checked_lease(keys)?;
    Ok(verified)
}

impl VerifiedTarget {
    /// The authorization's lease `(issued, expires)` after its check.
    pub fn checked_lease(&self, keys: &TrustedKeys) -> Result<(u64, u64)> {
        self.with_checked(keys, |checked, _| Ok((checked.issued_epoch(), checked.expires_epoch())))
    }

    fn with_checked<T>(
        &self,
        keys: &TrustedKeys,
        f: impl FnOnce(&crate::CheckedAuthorization<'_>, Option<&crate::CheckedRenewal<'_>>) -> Result<T>,
    ) -> Result<T> {
        match &self.auth {
            Auth::Launch { authorization, renewal } => {
                let checked = LaunchAuthorization::check(authorization, keys, &self.chain, &self.active)?;
                let renewal = renewal.as_ref().map(|r| RewardIntentRenewal::check(r, keys, &checked)).transpose()?;
                f(&checked, renewal.as_ref())
            }
            Auth::Cohort {
                authorization,
                renewal,
                carrier,
            } => {
                let source = self.source.as_ref().ok_or(Error::Invalid("cohort target without records"))?;
                let (manifest, package) = match carrier.as_deref() {
                    Some(c) => (&c.manifest, &c.package),
                    None => (&source.manifest, &source.package),
                };
                let checked =
                    RewardIntentAuthorization::check(authorization, keys, manifest, package, &self.target, &self.active)?;
                let renewal = renewal.as_ref().map(|r| RewardIntentRenewal::check(r, keys, &checked)).transpose()?;
                f(&checked, renewal.as_ref())
            }
        }
    }

    /// The per-tempo operation of validator `hotkey` at `epoch`, read from a
    /// fresh finalized view: the authorization is checked again, against the
    /// active target, its activation epoch and the lease.
    pub fn operation(&self, keys: &TrustedKeys, hotkey: AccountId, epoch: u64) -> Result<WeightOperationV2> {
        self.with_checked(keys, |checked, renewal| checked.operation(renewal, &self.active, hotkey, epoch))
    }

    /// The canonical row against the holder's own finalized snapshot.
    pub fn derive_row(&self, snapshot: &RegistrationSnapshotV2) -> Result<WeightRowV2> {
        match &self.source {
            None => crate::reward_v1::derive_row(&self.target, None, snapshot),
            Some(s) => crate::reward_v1::derive_row(&self.target, Some((&s.package, &s.index)), snapshot),
        }
    }

    /// Verifies retained receipt chunk objects against the source's index.
    pub fn verify_receipts(&self, chunks: &[&[u8]]) -> Result<u64> {
        let s = self.source.as_ref().ok_or(Error::Invalid("the burn row has no receipts"))?;
        let index = s.receipt_index.as_ref().ok_or(Error::Invalid("bundle carries no receipt index"))?;
        Ok(verify_receipt_chunks(index, chunks)?.len() as u64)
    }

    /// The carrying cohort's verified records, when a later cohort carries
    /// the target.
    pub fn carrier(&self) -> Option<&VerifiedCarrier> {
        match &self.auth {
            Auth::Cohort { carrier, .. } => carrier.as_deref(),
            Auth::Launch { .. } => None,
        }
    }

    /// Verifies the carrying cohort's retained receipt chunk objects against
    /// its receipt index.
    pub fn verify_carrier_receipts(&self, chunks: &[&[u8]]) -> Result<u64> {
        let c = self.carrier().ok_or(Error::Invalid("bundle has no carrier"))?;
        let index = c.receipt_index.as_ref().ok_or(Error::Invalid("carrier carries no receipt index"))?;
        Ok(verify_receipt_chunks(index, chunks)?.len() as u64)
    }
}
