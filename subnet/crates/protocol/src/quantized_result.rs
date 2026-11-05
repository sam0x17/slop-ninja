//! The public record of a scored cohort: [`QuantizedResult`].
//!
//! The coordinator's score package holds the exact scores and every hidden
//! measurement and never leaves its evaluation storage. What is published is
//! this record, signed with the package key: per entrant only its quantized
//! score `q` (or that it was withheld or not executed), its gate and
//! copy-gate bits, its cluster membership, its band rank and its units; the
//! references' `q`; the grid, the public reward parameters, the cutoff, the
//! occupied bands, the promoted identity and the counted bit budget.
//!
//! It carries no exact score, SE, offset, noise, statistic, probe value,
//! monitor, audit, attempt count, receipt value, stratum or source group.
//! Its `units` are a deterministic function of its other fields
//! ([`crate::reward_v1::derive_units`]), and [`QuantizedResult::validate`]
//! recomputes them, so anyone holding the record can recompute the row.

use crate::codec::{Canonical, Decoder, Encoder, canonical_struct};
use crate::quant::{self, Budget};
use crate::records::validate_label;
use crate::records_v2::{CutoffMode, PublicCopy, RewardV1Params, ScorePackageV2, tagged_enum};
use crate::reward_v1::{QEntrant, q_outcome};
use crate::sign::KeyRole;
use crate::{AccountId, ChainBinding, Digest, Error, MAX_ROSTER, Record, Result, RewardRule, Score};

tagged_enum! {
    /// The quantization rule a result follows.
    pub enum QuantRule ("quant_rule") {
        /// [`crate::quant`].
        QuantV1 = 1,
    }
}

impl QuantRule {
    pub fn name(self) -> &'static str {
        match self {
            Self::QuantV1 => quant::QUANT_RULE,
        }
    }
}

tagged_enum! {
    /// Why an entrant was not executed; both are decided at admission from
    /// public data.
    pub enum NotExecuted ("not_executed") {
        /// Its inference content is that of a published artifact.
        Content = 1,
        /// It failed qualification.
        Qualification = 2,
    }
}

/// What a result publishes about one entrant.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EntrantOutcome {
    NotExecuted(NotExecuted),
    /// The minimum-success gate failed: nothing else is published.
    Withheld,
    Scored {
        q: Score,
        /// No gate other than the public-copy gate failed.
        gate_pass: bool,
        /// The public-copy gate passed.
        copy_pass: bool,
        /// For an entrant at or above the cutoff, the roster index of its
        /// cluster's first member in roster order.
        cluster_first_member: Option<u16>,
    },
}

impl Canonical for EntrantOutcome {
    fn encode_to(&self, e: &mut Encoder) {
        match self {
            Self::NotExecuted(reason) => {
                e.u8(1);
                reason.encode_to(e);
            }
            Self::Withheld => e.u8(2),
            Self::Scored {
                q,
                gate_pass,
                copy_pass,
                cluster_first_member,
            } => {
                e.u8(3);
                q.encode_to(e);
                gate_pass.encode_to(e);
                copy_pass.encode_to(e);
                cluster_first_member.encode_to(e);
            }
        }
    }
    fn decode_from(d: &mut Decoder<'_>) -> Result<Self> {
        match d.u8()? {
            1 => Ok(Self::NotExecuted(NotExecuted::decode_from(d)?)),
            2 => Ok(Self::Withheld),
            3 => Ok(Self::Scored {
                q: Score::decode_from(d)?,
                gate_pass: bool::decode_from(d)?,
                copy_pass: bool::decode_from(d)?,
                cluster_first_member: Option::<u16>::decode_from(d)?,
            }),
            tag => Err(Error::InvalidTag {
                field: "entrant_outcome",
                tag,
            }),
        }
    }
}

impl EntrantOutcome {
    /// The published `q`.
    pub fn q(&self) -> Option<Score> {
        match self {
            Self::Scored { q, .. } => Some(*q),
            _ => None,
        }
    }

    /// Executed: scored or withheld.
    pub fn executed(&self) -> bool {
        !matches!(self, Self::NotExecuted(_))
    }
}

canonical_struct! {
    /// One roster entry of a result.
    pub struct ResultEntrant {
        pub uid: u16,
        pub hotkey: AccountId,
        pub registration_block: u64,
        /// The on-chain commitment, and the inference-content and artifact
        /// digests the entrant committed: all public before scoring.
        pub commitment: Digest,
        pub content: Digest,
        pub artifact: Digest,
        pub outcome: EntrantOutcome,
        /// `kappa` of the entrant's cluster, for a paid entrant.
        pub band_rank: Option<u16>,
        pub units: u16,
    }
}

canonical_struct! {
    /// A reference and its quantized score.
    #[derive(Copy)]
    pub struct ResultReference {
        pub content: Digest,
        pub artifact: Digest,
        pub q: Score,
    }
}

canonical_struct! {
    /// One occupied band, in absolute grid units: `[lower, upper)` with
    /// `lower = b W_q` and `upper = min((b + 1) W_q, 1)`.
    #[derive(Copy)]
    pub struct ResultBand {
        pub rank: u16,
        pub lower: Score,
        pub upper: Score,
    }
}

canonical_struct! {
    /// The counted bit budget ([`Budget`]) and its check
    /// `total * budget_factor <= cases * identify_bits`.
    #[derive(Copy)]
    pub struct BudgetRecord {
        pub executed: u16,
        pub grid_values: u64,
        pub per_entrant: u32,
        pub references: u32,
        pub clusters: u32,
        pub promotion: u32,
        pub width: u32,
        pub total: u32,
        pub within: bool,
    }
}

impl BudgetRecord {
    pub fn of(budget: &Budget, within: bool) -> Result<Self> {
        Ok(Self {
            executed: budget.executed,
            grid_values: u64::try_from(budget.grid_values).map_err(|_| Error::Overflow("grid values"))?,
            per_entrant: budget.per_entrant,
            references: budget.references,
            clusters: budget.clusters,
            promotion: budget.promotion,
            width: budget.width,
            total: budget.total,
            within,
        })
    }
}

canonical_struct! {
    /// The signed public result of one scored cohort (module docs).
    pub struct QuantizedResult {
        pub chain: ChainBinding,
        pub cohort: u64,
        pub cohort_label: String,
        pub cohort_manifest: Digest,
        /// The manifest's opening epoch.
        pub opened_epoch: u64,
        pub revision: u64,
        pub reward_rule: RewardRule,
        pub quant_rule: QuantRule,
        /// `n`.
        pub cases: u32,
        /// `m`, the charged values.
        pub charged: u32,
        /// `w`.
        pub width: u64,
        /// `W_q`; `None` when `q_Rk = 1`.
        pub band_width: Option<u64>,
        /// The manifest's public reward and quantization parameters.
        pub params: RewardV1Params,
        pub mode: CutoffMode,
        /// `T_q` or `C_q`; `None` without a target.
        pub cutoff: Option<Score>,
        pub reference_index: u64,
        pub current: ResultReference,
        /// Present exactly when `k > 0`.
        pub predecessor: Option<ResultReference>,
        /// `R_k+1`, a margin cohort's promoted cluster head.
        pub promoted: Option<AccountId>,
        /// Occupied bands, top first.
        pub bands: Vec<ResultBand>,
        /// Roster order.
        pub entrants: Vec<ResultEntrant>,
        pub budget: BudgetRecord,
    }
}

impl Record for QuantizedResult {
    const KIND: &'static str = "quantized-result";
    const VERSION: u16 = 1;
    const SIGNER: Option<KeyRole> = Some(KeyRole::Package);

    /// The structure, the grid, the charged count and every derived value:
    /// mode, cutoff, `W_q`, bands, band ranks and units are recomputed from
    /// the published `q` values, bits and membership; the bit budget is
    /// recounted and must be within its bound.
    fn validate(&self) -> Result<()> {
        validate_label(&self.cohort_label)?;
        self.reward_rule.check_reward_v2(&self.chain)?;
        self.params.validate()?;
        if self.entrants.len() > usize::from(MAX_ROSTER) {
            return Err(Error::LengthLimit {
                field: "entrants",
                len: self.entrants.len() as u64,
            });
        }
        if u64::from(self.params.quant.min_success_max_failed) >= u64::from(self.cases) {
            return Err(Error::OutOfRange("quant.min_success_max_failed"));
        }
        if (self.reference_index == 0) != self.predecessor.is_none() {
            return Err(Error::Invalid("reference predecessor"));
        }
        let executed = self.executed();
        let predecessor = self.predecessor.is_some();
        if self.charged != quant::charged(executed, predecessor) {
            return Err(Error::Mismatch("charged values"));
        }
        let tau = quant::Tau::new(self.cases, self.charged, self.params.quant.eps_total_milli)?;
        if !quant::on_ladder(self.width) || self.width < quant::width(0, &tau)? {
            return Err(Error::Invalid("grid width"));
        }
        for e in &self.entrants {
            if let EntrantOutcome::Scored { q, .. } = e.outcome
                && !quant::on_grid(q.get(), self.width)
            {
                return Err(Error::Invalid("q off the grid"));
            }
            if !matches!(e.outcome, EntrantOutcome::Scored { .. })
                && (e.band_rank.is_some() || e.units > 0)
            {
                return Err(Error::Invalid("unscored entrant placed"));
            }
        }
        let outcome = crate::reward_v1::derive_units(self)?;
        let cutoff = outcome.cutoff.map(Score::new).transpose()?;
        if outcome.mode != self.mode || cutoff != self.cutoff || outcome.band_width != self.band_width {
            return Err(Error::Mismatch("result outcome"));
        }
        if self.bands != bands_of(&outcome)? {
            return Err(Error::Mismatch("result bands"));
        }
        for (i, e) in self.entrants.iter().enumerate() {
            let rank = outcome.band_rank[i].map(|r| r as u16);
            if e.units != outcome.units[i] || e.band_rank != rank {
                return Err(Error::Mismatch("result units"));
            }
        }
        match (self.promoted, self.mode) {
            (None, _) => {}
            (Some(hotkey), CutoffMode::Margin) => {
                if !outcome.clusters.iter().any(|c| self.entrants[c.head].hotkey == hotkey) {
                    return Err(Error::Mismatch("promoted entrant"));
                }
            }
            (Some(_), _) => return Err(Error::Mismatch("promotion and cutoff mode")),
        }
        let budget = Budget::count(executed, self.width, predecessor)?;
        let within = budget.within(
            self.cases,
            self.params.quant.identify_bits,
            self.params.quant.budget_factor,
        );
        if self.budget != BudgetRecord::of(&budget, within)? {
            return Err(Error::Mismatch("bit budget"));
        }
        if !within {
            return Err(Error::Invalid("bit budget"));
        }
        Ok(())
    }
}

/// The occupied bands of an outcome, top first, in absolute grid units.
fn bands_of(outcome: &crate::reward_v1::QOutcome) -> Result<Vec<ResultBand>> {
    let Some(width) = outcome.band_width else {
        return Ok(Vec::new());
    };
    outcome
        .bands
        .iter()
        .enumerate()
        .map(|(rank, &b)| {
            let lower = u128::from(b) * u128::from(width);
            let upper = (lower + u128::from(width)).min(u128::from(crate::SCORE_SCALE));
            Ok(ResultBand {
                rank: rank as u16,
                lower: Score::new(lower as u64)?,
                upper: Score::new(upper as u64)?,
            })
        })
        .collect()
}

impl QuantizedResult {
    /// The result is the one `manifest` frames: its digest, chain, cohort,
    /// label, opening epoch, rule, parameters and batch size, its
    /// references, and its roster (identities and commitments in admission
    /// order).
    pub fn check_manifest(&self, manifest: &crate::CohortManifestV2) -> Result<()> {
        let refs = &manifest.references;
        let predecessor = match (&self.predecessor, &refs.predecessor) {
            (None, None) => true,
            (Some(r), Some(m)) => r.content == m.content && r.artifact == m.artifact,
            _ => false,
        };
        if self.cohort_manifest != manifest.digest()?
            || self.chain != manifest.chain
            || self.cohort != manifest.cohort
            || self.cohort_label != manifest.cohort_label
            || self.opened_epoch != manifest.opened_epoch
            || self.reward_rule != manifest.reward_rule
            || self.params != manifest.params
            || self.cases != manifest.batch.cases
            || self.reference_index != refs.index
            || self.current.content != refs.current.content
            || self.current.artifact != refs.current.artifact
            || !predecessor
        {
            return Err(Error::Mismatch("result manifest"));
        }
        if self.entrants.len() != manifest.roster.len()
            || self.entrants.iter().zip(&manifest.roster).any(|(e, r)| {
                e.uid != r.identity.uid
                    || e.hotkey != r.identity.hotkey
                    || e.registration_block != r.identity.registration_block
                    || e.commitment != r.commitment
            })
        {
            return Err(Error::Mismatch("result roster"));
        }
        Ok(())
    }

    /// `X`: executed entrants (scored or withheld).
    pub fn executed(&self) -> u16 {
        self.entrants.iter().filter(|e| e.outcome.executed()).count() as u16
    }

    /// The entrants as the q-only calculation reads them.
    pub fn q_entrants(&self) -> Vec<QEntrant> {
        self.entrants
            .iter()
            .map(|e| match e.outcome {
                EntrantOutcome::Scored {
                    q,
                    gate_pass,
                    copy_pass,
                    cluster_first_member,
                } => QEntrant {
                    q: Some(q.get()),
                    gated: gate_pass && copy_pass,
                    cluster: cluster_first_member,
                },
                _ => QEntrant {
                    q: None,
                    gated: false,
                    cluster: None,
                },
            })
            .collect()
    }

    /// The public result of a finalized package. It reads the package's
    /// quantized values and bits only.
    pub fn of_package(package: &ScorePackageV2) -> Result<Self> {
        let n = package.entrants.len();
        // Cluster membership: the first member in roster order of each head's
        // cluster.
        let mut first = vec![None; n];
        for (i, e) in package.entrants.iter().enumerate() {
            if let Some(head) = e.cluster_head {
                let label = package
                    .entrants
                    .iter()
                    .position(|x| x.cluster_head == Some(head))
                    .ok_or(Error::Mismatch("cluster head"))?;
                first[i] = Some(label as u16);
            }
        }
        let mut entrants = Vec::with_capacity(n);
        for (i, e) in package.entrants.iter().enumerate() {
            let outcome = match (e.score, e.q) {
                (None, _) => EntrantOutcome::NotExecuted(
                    if e.public_copy() == Some(PublicCopy::Content) {
                        NotExecuted::Content
                    } else {
                        NotExecuted::Qualification
                    },
                ),
                (Some(_), _) if e.withheld() => EntrantOutcome::Withheld,
                (Some(_), Some(q)) => EntrantOutcome::Scored {
                    q,
                    gate_pass: e.gate_pass(),
                    copy_pass: e.copy_pass(),
                    cluster_first_member: first[i],
                },
                (Some(_), None) => return Err(Error::Mismatch("quantized score coverage")),
            };
            entrants.push(ResultEntrant {
                uid: e.identity.uid,
                hotkey: e.identity.hotkey,
                registration_block: e.identity.registration_block,
                commitment: e.commitment,
                content: e.content,
                artifact: e.artifact,
                outcome,
                band_rank: None,
                units: e.units,
            });
        }
        let q = &package.quant;
        let reference = |model: &crate::ReferenceModel, q: Score| ResultReference {
            content: model.content,
            artifact: model.artifact,
            q,
        };
        let mut result = Self {
            chain: package.chain.clone(),
            cohort: package.cohort,
            cohort_label: package.cohort_label.clone(),
            cohort_manifest: package.cohort_manifest,
            opened_epoch: package.opened_epoch,
            revision: package.revision,
            reward_rule: package.reward_rule,
            quant_rule: QuantRule::QuantV1,
            cases: package.cases,
            charged: q.charged,
            width: q.width,
            band_width: q.band_width,
            params: package.params,
            mode: package.mode,
            cutoff: q.cutoff,
            reference_index: package.reference_index,
            current: reference(&package.reference.model, q.reference_q),
            predecessor: match (&package.predecessor, q.predecessor_q) {
                (Some(p), Some(pq)) => Some(reference(&p.model, pq)),
                (None, None) => None,
                _ => return Err(Error::Mismatch("predecessor q")),
            },
            promoted: package.promoted,
            bands: Vec::new(),
            entrants,
            budget: BudgetRecord::of(&Budget::default(), false)?,
        };
        let entrants_q = result.q_entrants();
        let outcome = q_outcome(&crate::reward_v1::QInput {
            params: &result.params,
            width: result.width,
            reference_q: result.current.q.get(),
            predecessor_q: result.predecessor.as_ref().map(|p| p.q.get()),
            entrants: &entrants_q,
        })?;
        result.bands = bands_of(&outcome)?;
        for (e, rank) in result.entrants.iter_mut().zip(&outcome.band_rank) {
            e.band_rank = rank.map(|r| r as u16);
        }
        let budget = Budget::count(result.executed(), result.width, result.predecessor.is_some())?;
        let within = budget.within(
            result.cases,
            result.params.quant.identify_bits,
            result.params.quant.budget_factor,
        );
        result.budget = BudgetRecord::of(&budget, within)?;
        Ok(result)
    }

    /// The longest signed record: [`MAX_ROSTER`] scored entrants in
    /// [`MAX_ROSTER`] bands, a predecessor, a promotion, a cutoff and the
    /// longest label. Readers refuse a larger declared length before
    /// allocating.
    pub fn max_signed_len() -> Result<u64> {
        let zero = Digest([0; 32]);
        let entrant = ResultEntrant {
            uid: 0,
            hotkey: AccountId([0; 32]),
            registration_block: 0,
            commitment: zero,
            content: zero,
            artifact: zero,
            outcome: EntrantOutcome::Scored {
                q: Score::ZERO,
                gate_pass: true,
                copy_pass: true,
                cluster_first_member: Some(0),
            },
            band_rank: Some(0),
            units: 0,
        };
        let band = ResultBand {
            rank: 0,
            lower: Score::ZERO,
            upper: Score::ZERO,
        };
        let reference = ResultReference {
            content: zero,
            artifact: zero,
            q: Score::ZERO,
        };
        let longest = Self {
            chain: ChainBinding {
                genesis: zero,
                netuid: 0,
                mechanism: 0,
            },
            cohort: 0,
            cohort_label: "a".repeat(crate::MAX_LABEL_LEN),
            cohort_manifest: zero,
            opened_epoch: 0,
            revision: 0,
            reward_rule: RewardRule::RewardV2,
            quant_rule: QuantRule::QuantV1,
            cases: 0,
            charged: 0,
            width: 0,
            band_width: Some(0),
            params: RewardV1Params::new(
                0,
                crate::QuantParams {
                    eps_total_milli: 0,
                    identify_bits: 0,
                    budget_factor: 0,
                    min_success_max_failed: 0,
                },
            ),
            mode: CutoffMode::Margin,
            cutoff: Some(Score::ZERO),
            reference_index: 0,
            current: reference,
            predecessor: Some(reference),
            promoted: Some(AccountId([0; 32])),
            bands: vec![band; usize::from(MAX_ROSTER)],
            entrants: vec![entrant; usize::from(MAX_ROSTER)],
            budget: BudgetRecord::of(&Budget::default(), true)?,
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
