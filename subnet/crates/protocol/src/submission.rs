//! Private `A` artifact submission.
//!
//! 1. A miner commits on chain only a digest: a domain-separated commitment
//!    binding the chain, cohort, its hotkey and registration generation, the
//!    canonical inference-content digest, and a fresh secret 256-bit nonce
//!    ([`SubmissionOpening::commitment`]). [`SubmissionOpening::chain_payload`]
//!    gives the exact bytes for the chain's commitment call.
//! 2. Before the admission cutoff it uploads the artifact object (see
//!    [`crate::artifact`]) with a [`SubmissionUpload`] envelope, privately, to
//!    the coordinator. The envelope carries the opening and is bound to the
//!    on-chain digest and to the exact artifact object. Admission
//!    ([`SubmissionUpload::admit`]) checks the envelope against the finalized
//!    commitment as read from the chain ([`FinalizedCommitment`]): this chain
//!    and cohort, made by the hotkey and registration generation the opening
//!    names. It derives the inference-content digest from the object,
//!    requires it to be the committed one, and returns the roster entry at
//!    the commitment's finalized block and extrinsic index.
//! 3. After the cohort closes, the coordinator publishes every entrant's
//!    artifact and opening in a signed [`CohortClosure`] bound to the score
//!    package, with the epochs at which admission closed, publication
//!    completed and the cohort closed. Validators check those epochs against
//!    the manifest's phase minimums and each opening against the package
//!    ([`CohortClosure::check`], which returns a [`CheckedClosure`]). For each
//!    entrant they check the finalized on-chain commitment: its committing
//!    registration and its block and extrinsic index are the package's, and
//!    the opening opens its payload. They also check the published object
//!    against the package's artifact digest and the committed content
//!    ([`CheckedClosure::check_entrant`]). They never run the models. Only a
//!    checked closure activates a cohort target
//!    ([`crate::ActiveTarget::activated`]), which a checked reward-intent
//!    authorization then commits ([`crate::CheckedAuthorization::operation`]).

use crate::codec::{Canonical, Encoder, canonical_struct};
use crate::hash::{domain_hash, object_sha256};
use crate::records::validate_label;
use crate::records_v2::{
    CalculationTargetV2, CohortManifestV2, CutoffMode, Nonce, RosterEntry, ScorePackageV2,
};
use crate::sign::KeyRole;
use crate::{
    AccountId, ChainBinding, Digest, Error, NeuronIdentity, Record, Result, VerifiedArtifact,
};

const COMMITMENT_DOMAIN: &str = "slop_ninja.protocol/submission-commitment/v1";
/// Leading tag of the on-chain commitment payload.
pub const SUBMISSION_PAYLOAD_TAG: [u8; 4] = *b"sna1";
/// `tag || u64_le(cohort) || commitment digest`.
pub const SUBMISSION_PAYLOAD_LEN: usize = 4 + 8 + 32;

canonical_struct! {
    /// The opening of a submission commitment. It stays private until the
    /// cohort closes.
    pub struct SubmissionOpening {
        pub chain: ChainBinding,
        pub cohort: u64,
        pub hotkey: AccountId,
        /// Registration generation of the credited hotkey.
        pub registration_block: u64,
        /// Canonical inference-content digest, derived from the artifact
        /// object's header.
        pub content: Digest,
        /// Fresh secret 256-bit nonce, so the digest reveals nothing about the
        /// content before the cohort ends.
        pub nonce: Nonce,
    }
}

impl SubmissionOpening {
    /// The digest a miner commits on chain.
    pub fn commitment(&self) -> Digest {
        let mut e = Encoder::new();
        self.encode_to(&mut e);
        domain_hash(COMMITMENT_DOMAIN, &e.into_bytes())
    }

    /// Exact bytes for the chain's commitment call:
    /// `"sna1" || u64_le(cohort) || commitment`.
    pub fn chain_payload(&self) -> [u8; SUBMISSION_PAYLOAD_LEN] {
        let mut out = [0u8; SUBMISSION_PAYLOAD_LEN];
        out[..4].copy_from_slice(&SUBMISSION_PAYLOAD_TAG);
        out[4..12].copy_from_slice(&self.cohort.to_le_bytes());
        out[12..].copy_from_slice(&self.commitment().0);
        out
    }

    /// Checks finalized on-chain commitment bytes against this opening.
    pub fn check_chain_payload(&self, payload: &[u8]) -> Result<()> {
        let (cohort, digest) = parse_chain_payload(payload)?;
        if cohort != self.cohort || digest != self.commitment() {
            return Err(Error::Mismatch("submission commitment"));
        }
        Ok(())
    }

    /// Checks a finalized on-chain commitment against this opening:
    ///
    /// - it was read from the opening's chain;
    /// - the committing account is the opening's hotkey under the opening's
    ///   registration generation, registered no later than the commitment
    ///   block;
    /// - the committed payload opens to this opening.
    ///
    /// Returns the roster entry at the commitment's finalized position: the
    /// committing registration with its UID at the commitment block, the
    /// commitment digest, and the block and extrinsic index.
    pub fn check_finalized(&self, commitment: &FinalizedCommitment<'_>) -> Result<RosterEntry> {
        if self.chain != *commitment.chain {
            return Err(Error::Mismatch("commitment chain"));
        }
        let committer = commitment.committer;
        if committer.hotkey != self.hotkey
            || committer.registration_block != self.registration_block
        {
            return Err(Error::Mismatch("committing account"));
        }
        if committer.registration_block > commitment.block {
            return Err(Error::Invalid("registration after commitment"));
        }
        self.check_chain_payload(commitment.payload)?;
        Ok(RosterEntry {
            identity: committer,
            commitment: self.commitment(),
            commitment_block: commitment.block,
            extrinsic_index: commitment.extrinsic_index,
        })
    }
}

/// Splits on-chain commitment bytes into the cohort and commitment digest.
pub fn parse_chain_payload(payload: &[u8]) -> Result<(u64, Digest)> {
    if payload.len() != SUBMISSION_PAYLOAD_LEN || payload[..4] != SUBMISSION_PAYLOAD_TAG {
        return Err(Error::Invalid("submission payload"));
    }
    let cohort = u64::from_le_bytes(payload[4..12].try_into().expect("8 bytes"));
    let digest = Digest(payload[12..].try_into().expect("32 bytes"));
    Ok((cohort, digest))
}

canonical_struct! {
    /// Private upload envelope sent to the coordinator's evaluation storage
    /// before the admission cutoff. It is bound to the on-chain digest and to
    /// the exact uploaded artifact object. Encryption to the coordinator key
    /// is a later, separately qualified layer around these bytes.
    pub struct SubmissionUpload {
        pub opening: SubmissionOpening,
        /// Must equal `opening.commitment()`.
        pub commitment: Digest,
        /// SHA-256 and length of the exact uploaded artifact object.
        pub artifact_sha256: Digest,
        pub artifact_len: u64,
    }
}

impl Record for SubmissionUpload {
    const KIND: &'static str = "submission-upload";
    const VERSION: u16 = 1;
    const SIGNER: Option<KeyRole> = None;
    fn validate(&self) -> Result<()> {
        if self.commitment != self.opening.commitment() {
            return Err(Error::Mismatch("upload commitment"));
        }
        if self.artifact_len == 0 {
            return Err(Error::Invalid("artifact length"));
        }
        Ok(())
    }
}

/// A finalized on-chain submission commitment as read from finalized state:
/// by the coordinator at admission, and by each validator when it checks the
/// published entrant ([`CheckedClosure::check_entrant`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FinalizedCommitment<'a> {
    /// The chain it was read from.
    pub chain: &'a ChainBinding,
    /// The registration (UID, hotkey and registration block) of the hotkey
    /// that signed the commitment call, as held at `block`.
    pub committer: NeuronIdentity,
    /// Finalized block of the commitment call.
    pub block: u64,
    /// Its extrinsic index within that block.
    pub extrinsic_index: u32,
    /// The committed bytes.
    pub payload: &'a [u8],
}

impl SubmissionUpload {
    /// The exact length of an encoded record (`Record::to_bytes`): every
    /// field is fixed-size, so the record has one length. The kind string
    /// (`u32` length and bytes), the `u16` version, the opening (chain:
    /// genesis, `u16` netuid, `u8` mechanism; `u64` cohort; hotkey; `u64`
    /// registration block; content digest; nonce), the commitment, the
    /// artifact digest and its `u64` length. The admission Workers bound a
    /// session-creation body by it.
    pub const ENCODED_LEN: usize = 4
        + <Self as Record>::KIND.len()
        + 2
        + (32 + 2 + 1)
        + 8
        + 32
        + 8
        + 32
        + 32
        + 32
        + 32
        + 8;

    pub fn new(opening: SubmissionOpening, artifact: &[u8]) -> Self {
        Self {
            commitment: opening.commitment(),
            opening,
            artifact_sha256: object_sha256(artifact),
            artifact_len: artifact.len() as u64,
        }
    }

    /// The coordinator's admission check for `cohort`:
    ///
    /// - the opening names `cohort`;
    /// - the finalized commitment passes
    ///   [`SubmissionOpening::check_finalized`]: read from the opening's
    ///   chain, made by the opening's hotkey under its registration
    ///   generation, registered no later than the commitment block, with a
    ///   payload that the envelope's opening opens;
    /// - the envelope carries exactly the verified `artifact` object, whose
    ///   derived inference content is the committed one.
    ///
    /// Returns the roster entry: the committing registration, the commitment
    /// digest, and the finalized block and extrinsic index that set admission
    /// order. Whether the commitment finalized before the admission cutoff,
    /// and qualification of the artifact's files, are checked separately.
    pub fn admit(
        &self,
        cohort: u64,
        commitment: &FinalizedCommitment<'_>,
        artifact: &VerifiedArtifact,
    ) -> Result<RosterEntry> {
        self.admit_digests(cohort, commitment, &artifact.sha256, artifact.len, &artifact.content)
    }

    /// [`SubmissionUpload::admit`] from the object's digests alone: its
    /// SHA-256 and length, and the inference-content digest a verifying
    /// reader derived from it. For a decider that receives those from the
    /// component that read the object, and never decodes the object itself.
    pub fn admit_digests(
        &self,
        cohort: u64,
        commitment: &FinalizedCommitment<'_>,
        object_sha256: &Digest,
        object_len: u64,
        content: &Digest,
    ) -> Result<RosterEntry> {
        self.validate()?;
        let opening = &self.opening;
        if opening.cohort != cohort {
            return Err(Error::Mismatch("commitment cohort"));
        }
        let entry = opening.check_finalized(commitment)?;
        if object_len != self.artifact_len || *object_sha256 != self.artifact_sha256 {
            return Err(Error::Mismatch("upload artifact"));
        }
        if *content != opening.content {
            return Err(Error::Mismatch("upload content"));
        }
        Ok(entry)
    }
}

canonical_struct! {
    /// One entrant's published opening; the other opening fields are the
    /// package's.
    pub struct PublishedOpening {
        pub hotkey: AccountId,
        pub registration_block: u64,
        pub nonce: Nonce,
    }
}

canonical_struct! {
    /// Signed record of a cohort's closing boundary: the epochs at which its
    /// phases with a minimum ended, the activation epoch of the cohort's
    /// target, and every entrant's commitment opening. The artifacts
    /// themselves are published as objects whose SHA-256 the score package
    /// binds. Epochs are finalized increments of the chain's epoch index.
    pub struct CohortClosure {
        pub chain: ChainBinding,
        pub cohort: u64,
        pub cohort_label: String,
        pub cohort_manifest: Digest,
        pub score_package: Digest,
        /// The cohort's new calculation target; `None` when it produced none
        /// and the previous target stays live.
        pub target: Option<Digest>,
        /// Epoch index of the boundary that closed admission and froze the
        /// roster: at least the admission minimum after the opening boundary.
        pub admission_closed_epoch: u64,
        /// Epoch index of the boundary at which publication completed and the
        /// transition began: after admission closed.
        pub published_epoch: u64,
        /// Epoch index of the closing boundary, at which `target` activates:
        /// at least the transition minimum after `published_epoch` and `K`
        /// tempos after the opening boundary.
        pub closed_epoch: u64,
        /// One per entrant, in the package's roster order.
        pub openings: Vec<PublishedOpening>,
    }
}

impl Record for CohortClosure {
    const KIND: &'static str = "cohort-closure";
    const VERSION: u16 = 1;
    const SIGNER: Option<KeyRole> = Some(KeyRole::Package);
    fn validate(&self) -> Result<()> {
        validate_label(&self.cohort_label)?;
        crate::records::bounded(&self.openings, "openings")
    }
}

canonical_struct! {
    /// Signed, content-free record that the coordinator ended a cohort
    /// without a target (an operator's abandonment of a faulted cohort): no
    /// closure will ever follow, and the previous calculation target stays
    /// live. It lets a reader of the cohort sequence (the automatic target
    /// handoff) move past the cohort with no operator mark of its own.
    /// `fault` is the code the cohort was paused by and `reason` the
    /// operator's code (each lowercase ASCII letters, digits and `_-.:`, at
    /// most [`ABANDONMENT_CODE_MAX`] bytes). `cohort_manifest` is absent for
    /// a cohort abandoned before its seal; `abandoned_epoch` is the epoch
    /// index observed when it was abandoned, absent when none was observed.
    pub struct CohortAbandonment {
        pub chain: ChainBinding,
        pub cohort: u64,
        pub cohort_label: String,
        pub cohort_manifest: Option<Digest>,
        pub fault: String,
        pub reason: String,
        pub abandoned_epoch: Option<u64>,
    }
}

/// The longest `fault` or `reason` code of a [`CohortAbandonment`].
pub const ABANDONMENT_CODE_MAX: usize = 64;

fn abandonment_code(code: &str, what: &'static str) -> Result<()> {
    if code.is_empty()
        || code.len() > ABANDONMENT_CODE_MAX
        || !code
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"_-.:".contains(&b))
    {
        return Err(Error::Invalid(what));
    }
    Ok(())
}

impl Record for CohortAbandonment {
    const KIND: &'static str = "cohort-abandonment";
    const VERSION: u16 = 1;
    const SIGNER: Option<KeyRole> = Some(KeyRole::Package);
    fn validate(&self) -> Result<()> {
        validate_label(&self.cohort_label)?;
        abandonment_code(&self.fault, "abandonment fault")?;
        abandonment_code(&self.reason, "abandonment reason")
    }
}

impl CohortClosure {
    /// Checks the closure against the manifest's schedule (the admission and
    /// transition minimums, publication after admission, and the cohort
    /// minimum `K`), the package it closes and that package's target, and
    /// every opening against the package's commitments.
    ///
    /// Returns the [`CheckedClosure`], the only form in which a closure
    /// activates its target ([`crate::ActiveTarget::activated`]) or checks an
    /// entrant against its finalized on-chain commitment
    /// ([`CheckedClosure::check_entrant`]). A closure is therefore never
    /// used before its phase epochs are checked against the manifest's
    /// frozen minimums.
    pub fn check<'a>(
        &'a self,
        manifest: &CohortManifestV2,
        package: &'a ScorePackageV2,
        target: Option<&CalculationTargetV2>,
    ) -> Result<CheckedClosure<'a>> {
        self.validate()?;
        package.check_manifest(manifest)?;
        if self.chain != package.chain
            || self.cohort != package.cohort
            || self.cohort_label != package.cohort_label
            || self.cohort_manifest != package.cohort_manifest
            || self.score_package != package.digest()?
        {
            return Err(Error::Mismatch("closure package"));
        }
        if self.admission_closed_epoch < manifest.earliest_admission_close_epoch()? {
            return Err(Error::Invalid("admission closed before its minimum"));
        }
        if self.published_epoch <= self.admission_closed_epoch {
            return Err(Error::Invalid("published before admission closed"));
        }
        let transition_end = self
            .published_epoch
            .checked_add(u64::from(manifest.schedule.transition_min))
            .ok_or(Error::Overflow("transition end"))?;
        if self.closed_epoch < transition_end {
            return Err(Error::Invalid("closed before the transition minimum"));
        }
        if self.closed_epoch < manifest.earliest_close_epoch()? {
            return Err(Error::Invalid("closed before the cohort minimum"));
        }
        match (self.target, target, package.mode) {
            (None, None, CutoffMode::NoTarget) => {}
            (Some(digest), Some(t), CutoffMode::Margin | CutoffMode::Fallback) => {
                t.check_package(package)?;
                if t.digest()? != digest {
                    return Err(Error::Mismatch("closure target"));
                }
            }
            _ => return Err(Error::Mismatch("closure target")),
        }
        if self.openings.len() != package.entrants.len() {
            return Err(Error::Mismatch("closure openings"));
        }
        for index in 0..self.openings.len() {
            let opening = self.opening(package, index)?;
            if opening.commitment() != package.entrants[index].commitment {
                return Err(Error::Mismatch("opening commitment"));
            }
        }
        Ok(CheckedClosure {
            closure: self,
            package,
        })
    }

    /// The full opening of the `index`th entrant.
    pub fn opening(&self, package: &ScorePackageV2, index: usize) -> Result<SubmissionOpening> {
        let published = self
            .openings
            .get(index)
            .ok_or(Error::OutOfRange("opening index"))?;
        let entrant = package
            .entrants
            .get(index)
            .ok_or(Error::OutOfRange("opening index"))?;
        if published.hotkey != entrant.identity.hotkey
            || published.registration_block != entrant.identity.registration_block
        {
            return Err(Error::Mismatch("opening identity"));
        }
        Ok(SubmissionOpening {
            chain: package.chain.clone(),
            cohort: package.cohort,
            hotkey: published.hotkey,
            registration_block: published.registration_block,
            content: entrant.content,
            nonce: published.nonce,
        })
    }
}

/// A [`CohortClosure`] that [`CohortClosure::check`] accepted against its
/// manifest, the package it closes and that package's target, including the
/// phase epochs against the manifest's frozen minimums. It borrows the closure
/// and the checked package; a validator that journals a closure checks it
/// again after reloading it.
#[derive(Clone, Copy, Debug)]
pub struct CheckedClosure<'a> {
    closure: &'a CohortClosure,
    package: &'a ScorePackageV2,
}

impl<'a> CheckedClosure<'a> {
    /// The checked closure.
    pub fn closure(&self) -> &'a CohortClosure {
        self.closure
    }

    /// The package it was checked against.
    pub fn package(&self) -> &'a ScorePackageV2 {
        self.package
    }

    /// The validator's check of the `index`th entrant's publication against
    /// `commitment`, the finalized on-chain commitment as the validator reads
    /// it from its own finalized view:
    ///
    /// - the commitment passes [`SubmissionOpening::check_finalized`] for the
    ///   entrant's opening;
    /// - its committing registration, UID included, is the package's entrant
    ///   identity;
    /// - its finalized block and extrinsic index are the entrant's position,
    ///   which breaks rank-order ties;
    /// - the published `artifact` object is the package's and carries the
    ///   committed inference content.
    pub fn check_entrant(
        &self,
        index: usize,
        commitment: &FinalizedCommitment<'_>,
        artifact: &VerifiedArtifact,
    ) -> Result<()> {
        let opening = self.closure.opening(self.package, index)?;
        let entrant = &self.package.entrants[index];
        let entry = opening.check_finalized(commitment)?;
        if entry.commitment != entrant.commitment {
            return Err(Error::Mismatch("opening commitment"));
        }
        if entry.identity != entrant.identity {
            return Err(Error::Mismatch("committing uid"));
        }
        if entry.position() != entrant.position() {
            return Err(Error::Mismatch("commitment position"));
        }
        check_published_artifact(self.package, &entrant.identity.hotkey, artifact)
    }
}

/// Checks one published artifact object against the package: the object is
/// the one the package names, and its derived inference content is the
/// entrant's content, which the entrant's opening commits to on chain.
pub fn check_published_artifact(
    package: &ScorePackageV2,
    hotkey: &AccountId,
    artifact: &VerifiedArtifact,
) -> Result<()> {
    let entrant = package
        .entrant(hotkey)
        .ok_or(Error::Mismatch("published artifact entrant"))?;
    if artifact.sha256 != entrant.artifact {
        return Err(Error::Mismatch("published artifact"));
    }
    if artifact.content != entrant.content {
        return Err(Error::Mismatch("published artifact content"));
    }
    Ok(())
}
