//! The coordinator's signed row: what every other validator commits.
//!
//! Under the copy model (user decision of 2026-11-04) the coordinator alone
//! measures entrants, computes the reward outcome and derives the native row.
//! When it commits a row it signs a [`CoordinatorRow`] with the
//! authorization key and publishes it off-chain beside the authorization it
//! committed under. Every other validator verifies that record and commits
//! exactly its entries through its own journaled commit-reveal. No validator
//! receives a score package, a receipt or anything else computed from the
//! hidden evaluation, and nothing here recomputes the outcome. The one
//! change a validator applies is the registration rule of
//! [`VerifiedRow::derive_row`]: units of a registration its own snapshot no
//! longer holds burn, as the coordinator's own derivation burns them.
//!
//! The record names:
//!
//! - the chain, netuid and mechanism;
//! - the cohort whose authorization it was committed under (its number,
//!   label and manifest digest), or the launch burn row;
//! - the calculation target, by digest, and the epoch its closure activated
//!   it at (none for the launch burn row);
//! - the authorization record, by digest, and that authorization's issue
//!   epoch;
//! - the epoch of the coordinator's commitment and the finalized block of the
//!   registrations it read;
//! - the owner hotkey of the coordinator's snapshot (the entry that pays it
//!   is the burn);
//! - the canonical row: `(uid, hotkey, registration block, weight)` sorted
//!   by UID, positive weights summing to 65,535.
//!
//! [`verify`] opens a record with the authorization it names (and any hint
//! refresh of it) for a validator that holds `held` active. The rules that do
//! not need the cohort's records stay: both records verify under the trusted
//! authorization key and name this chain; the row names the authorization's
//! digest, target, cohort and issue; the launch authorization only while the
//! held target is still the launch burn row; a cohort target only when it is
//! the held one (same activation epoch) or one activated later. Nothing
//! expires: [`VerifiedRow::operation`] refuses only a target that is no
//! longer the held one, an epoch before the target's activation or the
//! authorization's issue, and an authorization a journaled signed revocation
//! withdraws. A silent coordinator therefore never stops validators from
//! committing its last signed row.
//!
//! [`VerifiedRow::derive_row`] returns the record's entries as the native
//! row, with one deterministic, score-free rule applied against the
//! validator's own finalized snapshot, the same one the coordinator's
//! derivation applies to an entrant whose registration changed: an entry
//! whose `(uid, hotkey, registration block)` the snapshot no longer holds
//! (a deregistration, a re-registration of the same hotkey, the UID handed
//! to another hotkey), and the burn entry, go to the UID the snapshot's
//! owner hotkey holds; when units must burn and the owner hotkey holds no
//! UID, the tempo's row is deferred ([`Error::Deferred`]), never sent to
//! another UID. A UID that changed hands is therefore never paid to its new
//! holder, during coordinator silence included. No weight is otherwise
//! changed and nothing is recomputed from scores.

use crate::authorization::{
    ActiveTarget, LAUNCH_RENEW_BY_TEMPOS, LaunchAuthorization, RewardIntentAuthorization, RewardIntentRenewal,
    RewardIntentRevocation,
};
use crate::codec::{Canonical, Decoder, Encoder, canonical_struct};
use crate::records::{strictly_increasing, validate_label};
use crate::records_v2::{CalculationTargetV2, ROW_TOTAL, RegistrationSnapshotV2, WeightRowV2};
use crate::sign::{KeyRole, Signed, TrustedKeys};
use crate::{AccountId, ChainBinding, Digest, Error, Record, Result, RewardRule, RowEntry, WeightOperationV2};

/// Largest number of row entries: a `reward-v1` row pays entrants of one
/// roster (at most [`crate::MAX_ROSTER`]) and burns the rest through one
/// owner UID, so it has at most one entry more than the roster bound.
pub const MAX_ROW_ENTRIES: usize = crate::MAX_ROSTER as usize + 1;

canonical_struct! {
    /// One entry of a coordinator row: the UID, the hotkey and registration
    /// block the coordinator's finalized snapshot read at that UID, and its
    /// canonical weight.
    #[derive(Copy)]
    pub struct CoordinatorRowEntry {
        pub uid: u16,
        pub hotkey: AccountId,
        pub registration_block: u64,
        pub weight: u16,
    }
}

/// What the coordinator committed the row under.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RowSource {
    /// The launch authorization of the launch burn row.
    LaunchBurn,
    /// A cohort's reward-intent authorization (the cohort that carries it).
    Cohort {
        cohort: u64,
        cohort_label: String,
        cohort_manifest: Digest,
    },
}

impl Canonical for RowSource {
    fn encode_to(&self, e: &mut Encoder) {
        match self {
            Self::LaunchBurn => e.u8(1),
            Self::Cohort {
                cohort,
                cohort_label,
                cohort_manifest,
            } => {
                e.u8(2);
                cohort.encode_to(e);
                cohort_label.encode_to(e);
                cohort_manifest.encode_to(e);
            }
        }
    }
    fn decode_from(d: &mut Decoder<'_>) -> Result<Self> {
        match d.u8()? {
            1 => Ok(Self::LaunchBurn),
            2 => Ok(Self::Cohort {
                cohort: u64::decode_from(d)?,
                cohort_label: String::decode_from(d)?,
                cohort_manifest: Digest::decode_from(d)?,
            }),
            tag => Err(Error::InvalidTag {
                field: "row_source",
                tag,
            }),
        }
    }
}

canonical_struct! {
    /// The row the coordinator committed at `epoch`, signed with the
    /// authorization key and published off-chain when it commits (module
    /// docs). Validators commit its entries and nothing else.
    pub struct CoordinatorRow {
        pub chain: ChainBinding,
        pub reward_rule: RewardRule,
        pub source: RowSource,
        /// Digest of the calculation target the row pays.
        pub target: Digest,
        /// The closing epoch of the closure that activated `target`; `None`
        /// for the launch burn row.
        pub activated_epoch: Option<u64>,
        /// Digest of the authorization record the row was committed under.
        pub authorization: Digest,
        /// That authorization's first epoch.
        pub issued_epoch: u64,
        /// The chain's epoch index of the coordinator's commitment.
        pub epoch: u64,
        /// The finalized block of the registrations the row was read at.
        pub registration_block: u64,
        /// The subnet owner hotkey of that snapshot: an entry paying it is
        /// the burn, which validators send to the UID their own snapshot's
        /// owner hotkey holds.
        pub owner_hotkey: AccountId,
        /// Sorted by UID; positive weights summing to exactly 65,535.
        pub entries: Vec<CoordinatorRowEntry>,
    }
}

impl Record for CoordinatorRow {
    const KIND: &'static str = "coordinator-row";
    const VERSION: u16 = 1;
    const SIGNER: Option<KeyRole> = Some(KeyRole::Authorization);
    fn validate(&self) -> Result<()> {
        self.reward_rule.check_reward_v1(&self.chain)?;
        if let RowSource::Cohort { cohort_label, .. } = &self.source {
            validate_label(cohort_label)?;
        }
        let launch = CalculationTargetV2::launch_burn(self.chain.clone()).digest()?;
        // The launch burn row is the only target without an activation; a
        // no-target cohort's authorization may carry it before any
        // activation.
        if self.activated_epoch.is_none() != (self.target == launch) {
            return Err(Error::Invalid("row activation"));
        }
        if self.source == RowSource::LaunchBurn && self.target != launch {
            return Err(Error::Mismatch("launch row target"));
        }
        if self.epoch < self.issued_epoch || self.activated_epoch.is_some_and(|a| self.epoch < a) {
            return Err(Error::Invalid("row epoch"));
        }
        if self.entries.len() > MAX_ROW_ENTRIES {
            return Err(Error::LengthLimit {
                field: "row entries",
                len: self.entries.len() as u64,
            });
        }
        let uids: Vec<u16> = self.entries.iter().map(|e| e.uid).collect();
        strictly_increasing(&uids, "row uid")?;
        let mut hotkeys: Vec<AccountId> = self.entries.iter().map(|e| e.hotkey).collect();
        hotkeys.sort_unstable();
        strictly_increasing(&hotkeys, "row hotkey")?;
        if self.entries.iter().any(|e| e.weight == 0) {
            return Err(Error::Invalid("zero row weight"));
        }
        if self.entries.iter().map(|e| u32::from(e.weight)).sum::<u32>() != u32::from(ROW_TOTAL) {
            return Err(Error::Invalid("row total"));
        }
        Ok(())
    }
}

impl CoordinatorRow {
    /// The longest signed record of `entries` entries (every entry has one
    /// length), for readers that bound a row object before reading it.
    pub fn signed_len(entries: usize) -> Result<u64> {
        let chain = ChainBinding {
            genesis: Digest([0; 32]),
            netuid: 0,
            mechanism: 0,
        };
        // The longest source: a cohort label at the label bound.
        let label_len = crate::MAX_LABEL_LEN;
        let empty = Self {
            chain,
            reward_rule: RewardRule::RewardV1,
            source: RowSource::Cohort {
                cohort: 0,
                cohort_label: "a".repeat(label_len),
                cohort_manifest: Digest([0; 32]),
            },
            target: Digest([0; 32]),
            activated_epoch: Some(0),
            authorization: Digest([0; 32]),
            issued_epoch: 0,
            epoch: 0,
            registration_block: 0,
            owner_hotkey: AccountId([0; 32]),
            entries: Vec::new(),
        };
        let mut e = Encoder::new();
        e.str(Self::KIND);
        e.u16(Self::VERSION);
        empty.encode_to(&mut e);
        let record = e.into_bytes().len() as u64;
        let mut e = Encoder::new();
        CoordinatorRowEntry {
            uid: 0,
            hotkey: AccountId([0; 32]),
            registration_block: 0,
            weight: 0,
        }
        .encode_to(&mut e);
        let entry = e.into_bytes().len() as u64;
        // Envelope: str("signed") || u16 || u8 || u32 len || record || key || signature.
        let envelope = 4 + 6 + 2 + 1 + 4 + 32 + 64;
        Ok(envelope + record + entry * entries as u64)
    }

    /// The bound every reader applies to a row object: a row of
    /// [`MAX_ROW_ENTRIES`] entries with the longest label.
    pub fn max_signed_len() -> Result<u64> {
        Self::signed_len(MAX_ROW_ENTRIES)
    }

    /// The row's entries as the native row's `(uid, weight)` entries.
    pub fn row_entries(&self) -> Vec<RowEntry> {
        self.entries.iter().map(|e| RowEntry { uid: e.uid, weight: e.weight }).collect()
    }
}

/// The authorization a row bundle carries, as signed bytes.
#[derive(Clone, Copy, Debug)]
pub enum RowAuthorizing<'a> {
    Launch(&'a [u8]),
    Cohort(&'a [u8]),
}

#[derive(Clone, Debug)]
enum RowAuth {
    Launch(LaunchAuthorization),
    Cohort(RewardIntentAuthorization),
}

/// A coordinator row opened with the authorization it names, for a holder of
/// one active target ([`verify`]).
#[derive(Clone, Debug)]
pub struct VerifiedRow {
    pub chain: ChainBinding,
    pub row: CoordinatorRow,
    /// The target active after this bundle: the held target, or the later
    /// target the row names. The holder records it durably.
    pub active: ActiveTarget,
    /// Digest of the authorization record.
    pub authorization_digest: Digest,
    auth: RowAuth,
    renewal: Option<RewardIntentRenewal>,
}

/// Opens a coordinator row, its authorization and any hint refresh of it for
/// a holder on `chain` whose own durable record holds `held` active (module
/// docs). Every refusal happens before a row exists.
pub fn verify(
    chain: &ChainBinding,
    keys: &TrustedKeys,
    held: &ActiveTarget,
    authorization: RowAuthorizing<'_>,
    renewal: Option<&[u8]>,
    row: &[u8],
) -> Result<VerifiedRow> {
    if held.chain() != chain {
        return Err(Error::Mismatch("held target chain"));
    }
    let row = Signed::<CoordinatorRow>::open(row, keys)?;
    if row.chain != *chain {
        return Err(Error::Mismatch("row chain"));
    }
    let launch = CalculationTargetV2::launch_burn(chain.clone()).digest()?;
    let (auth, digest) = match authorization {
        RowAuthorizing::Launch(bytes) => {
            let a = Signed::<LaunchAuthorization>::open(bytes, keys)?;
            if a.chain != *chain || a.target != launch {
                return Err(Error::Mismatch("authorization chain or target"));
            }
            if a.renew_by_epoch - a.issued_epoch != LAUNCH_RENEW_BY_TEMPOS {
                return Err(Error::Mismatch("authorization renew-by hint"));
            }
            if row.source != RowSource::LaunchBurn {
                return Err(Error::Mismatch("row source"));
            }
            let digest = a.digest()?;
            (RowAuth::Launch(a), digest)
        }
        RowAuthorizing::Cohort(bytes) => {
            let a = Signed::<RewardIntentAuthorization>::open(bytes, keys)?;
            if a.chain != *chain {
                return Err(Error::Mismatch("authorization chain"));
            }
            match &row.source {
                RowSource::Cohort {
                    cohort,
                    cohort_manifest,
                    ..
                } if *cohort == a.cohort && *cohort_manifest == a.cohort_manifest => {}
                _ => return Err(Error::Mismatch("row source")),
            }
            let digest = a.digest()?;
            (RowAuth::Cohort(a), digest)
        }
    };
    let (target, issued, generation) = match &auth {
        RowAuth::Launch(a) => (a.target, a.issued_epoch, a.checkpoint_generation),
        RowAuth::Cohort(a) => (a.target, a.issued_epoch, a.checkpoint_generation),
    };
    if row.authorization != digest || row.target != target || row.issued_epoch != issued {
        return Err(Error::Mismatch("row authorization"));
    }
    let renewal = match renewal {
        None => None,
        Some(bytes) => {
            let r = Signed::<RewardIntentRenewal>::open(bytes, keys)?;
            if r.authorization != digest {
                return Err(Error::Mismatch("renewed authorization"));
            }
            if r.checkpoint_generation < generation {
                return Err(Error::Invalid("renewal checkpoint generation"));
            }
            if r.issued_epoch < issued {
                return Err(Error::Invalid("renewal before its authorization"));
            }
            Some(r)
        }
    };
    let active = match row.activated_epoch {
        None => {
            if !held.is_launch_burn() {
                return Err(Error::Invalid("launch burn row after activation"));
            }
            held.clone()
        }
        Some(activated) => {
            if matches!(auth, RowAuth::Launch(_)) {
                return Err(Error::Invalid("launch authorization of a cohort target"));
            }
            if row.target == held.digest() {
                if held.activated_epoch() != Some(activated) {
                    return Err(Error::Mismatch("held target activation epoch"));
                }
                held.clone()
            } else {
                if held.activated_epoch().is_some_and(|h| activated <= h) {
                    return Err(Error::Invalid("row target activates no later than the held target"));
                }
                ActiveTarget::restore(chain, row.target, Some(activated))?
            }
        }
    };
    Ok(VerifiedRow {
        chain: chain.clone(),
        row,
        active,
        authorization_digest: digest,
        auth,
        renewal,
    })
}

impl VerifiedRow {
    /// Digest of the target the row pays.
    pub fn target_digest(&self) -> Digest {
        self.row.target
    }

    /// Whether the row was committed under the launch authorization.
    pub fn is_launch(&self) -> bool {
        matches!(self.auth, RowAuth::Launch(_))
    }

    /// The authorization's identity and epochs.
    pub fn terms(&self) -> crate::AuthorizationTerms {
        let (generation, issued, renew_by) = match &self.auth {
            RowAuth::Launch(a) => (a.checkpoint_generation, a.issued_epoch, a.renew_by_epoch),
            RowAuth::Cohort(a) => (a.checkpoint_generation, a.issued_epoch, a.renew_by_epoch),
        };
        crate::AuthorizationTerms {
            digest: self.authorization_digest,
            checkpoint_generation: generation,
            issued_epoch: issued,
            renew_by_epoch: self.renewal.as_ref().map_or(0, |r| r.renew_by_epoch).max(renew_by),
        }
    }

    /// The first of the holder's journaled `revocations` that withdraws this
    /// row's authorization: the same chain and target, issued at or before
    /// the revocation ([`crate::CheckedRevocation::withdraws`]).
    pub fn revoked_by(&self, revocations: &[RewardIntentRevocation]) -> Result<Option<Digest>> {
        let issued = self.row.issued_epoch;
        for r in revocations {
            if r.chain == self.chain && r.target == self.row.target && issued <= r.issued_epoch {
                return Ok(Some(r.digest()?));
            }
        }
        Ok(None)
    }

    /// The per-tempo operation of validator `hotkey` at `epoch`, against the
    /// target the holder holds active then and its journaled revocations. No
    /// end epoch applies.
    pub fn operation(
        &self,
        revocations: &[RewardIntentRevocation],
        active: &ActiveTarget,
        hotkey: AccountId,
        epoch: u64,
    ) -> Result<WeightOperationV2> {
        if self.is_launch() && !active.is_launch_burn() {
            return Err(Error::Invalid("launch authorization after activation"));
        }
        if active.chain() != &self.chain || active.digest() != self.row.target {
            return Err(Error::Mismatch("authorized target not active"));
        }
        if active.activated_epoch().is_some_and(|a| epoch < a) {
            return Err(Error::Invalid("epoch before target activation"));
        }
        if epoch < self.row.issued_epoch {
            return Err(Error::Invalid("epoch before authorization"));
        }
        if self.revoked_by(revocations)?.is_some() {
            return Err(Error::Revoked("reward intent authorization"));
        }
        Ok(WeightOperationV2 {
            genesis: self.chain.genesis,
            netuid: self.chain.netuid,
            mechanism: self.chain.mechanism,
            hotkey,
            target: self.row.target,
            epoch,
        })
    }

    /// The native row: the record's entries, with the module's one rule
    /// applied against the holder's own finalized `snapshot`: the burn entry
    /// and every entry whose `(uid, hotkey, registration block)` the
    /// snapshot does not hold go to the UID the snapshot's owner hotkey
    /// holds ([`Error::Deferred`] when units must burn and it holds none).
    /// The snapshot also supplies the row's registration block and digest
    /// and the owner hotkey. Nothing else changes a UID or a weight.
    pub fn derive_row(&self, snapshot: &RegistrationSnapshotV2) -> Result<WeightRowV2> {
        if snapshot.chain != self.chain {
            return Err(Error::Mismatch("registration chain"));
        }
        let owner_uid = snapshot.by_hotkey(&snapshot.owner_hotkey).map(|n| n.uid);
        let burn = || owner_uid.ok_or(Error::Deferred("owner hotkey not registered"));
        let mut units = std::collections::BTreeMap::<u16, u32>::new();
        for e in &self.row.entries {
            let uid = if e.hotkey != self.row.owner_hotkey && self.holds(snapshot, e) {
                e.uid
            } else {
                burn()?
            };
            *units.entry(uid).or_default() += u32::from(e.weight);
        }
        let entries = units
            .into_iter()
            .map(|(uid, weight)| {
                Ok(RowEntry {
                    uid,
                    weight: u16::try_from(weight).map_err(|_| Error::Overflow("row weight"))?,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        let row = WeightRowV2 {
            chain: self.chain.clone(),
            calculation_target: self.row.target,
            reward_rule: self.row.reward_rule,
            registration_block: snapshot.block,
            registration: snapshot.digest()?,
            owner_hotkey: snapshot.owner_hotkey,
            entries,
        };
        row.validate()?;
        Ok(row)
    }

    /// Whether the holder's snapshot still holds `e`'s registration.
    fn holds(&self, snapshot: &RegistrationSnapshotV2, e: &CoordinatorRowEntry) -> bool {
        snapshot
            .neurons
            .iter()
            .any(|n| n.uid == e.uid && n.hotkey == e.hotkey && n.registration_block == e.registration_block)
    }

    /// Paid entries (not the burn) whose registration the holder's snapshot
    /// no longer holds: [`Self::derive_row`] sends their units to the
    /// owner's UID. For the holder's logs.
    pub fn moved_uids(&self, snapshot: &RegistrationSnapshotV2) -> Vec<u16> {
        self.row
            .entries
            .iter()
            .filter(|e| e.hotkey != self.row.owner_hotkey && !self.holds(snapshot, e))
            .map(|e| e.uid)
            .collect()
    }
}

impl CoordinatorRow {
    // QUANTIZED-SCORE: the signed row carries the weights derived from exact
    // scores ([`crate::reward_v1::derive_row`]); it reaches validators (and
    // the public host) when the coordinator derives it, before the reveal.
    /// The coordinator's record of the row it derived at `epoch` from the
    /// registrations read at `registration_block`, under its own verified
    /// bundle `v`: the bundle's chain, target and activation, the source and
    /// terms of its checked authorization.
    pub fn of_verified(
        v: &crate::VerifiedTarget,
        keys: &TrustedKeys,
        epoch: u64,
        registration_block: u64,
        owner_hotkey: AccountId,
        entries: Vec<CoordinatorRowEntry>,
    ) -> Result<Self> {
        let terms = v.checked_terms(keys)?;
        let row = Self {
            chain: v.chain.clone(),
            reward_rule: RewardRule::RewardV1,
            source: v.row_source()?,
            target: v.target_digest,
            activated_epoch: v.active.activated_epoch(),
            authorization: terms.digest,
            issued_epoch: terms.issued_epoch,
            epoch,
            registration_block,
            owner_hotkey,
            entries,
        };
        row.validate()?;
        Ok(row)
    }
}
