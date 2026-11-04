//! The public summary of one closed cohort
//! (`v1/cohorts/<label>/summary.json`, schema
//! `slop_ninja.public.cohort_summary/v2`): the only cohort object the public
//! host carries.
//!
//! The coordinator keeps every record computed from the hidden evaluation
//! private: the signed manifest (its strata and source groups), the score
//! package (scores, near-copy statistics, probe matches, the distance
//! monitor, promotion audits, attempt counts, the receipt root and count),
//! the target, the closure, the published-content and fingerprint indexes,
//! the components and every receipt. The publisher verifies them in the
//! private bucket and publishes only this allow-listed summary:
//!
//! - per entrant, in roster (admission) order: its UID and hotkey, its
//!   reward band (`1` is the top occupied band), one gate bit
//!   (`pass`, `fail`, or `not_executed` for an entrant never scored), one
//!   public-copy gate bit and its near-copy cluster membership (named by the
//!   cluster's first member in roster order);
//! - `R_k` and `R_k-1` as the cohort's manifest named them, by content and
//!   artifact digest, and the entrant promoted to `R_k+1`, if any;
//! - the cohort's number, label and opening and closing epochs.
//!
//! No score value and no rank is published, not even rounded (the user's
//! quantization specification is pending). Every place that will carry the
//! quantized value once it is approved is marked `QUANTIZED-SCORE`; until
//! then a band is all a reader learns about a model's measured quality.
//!
//! No order among entrants is published either. A rank, a list in rank
//! order or a cluster named by its head (the cluster's best-scoring member)
//! would each be an ordering an entrant with several hotkeys can choose: up
//! to `log2(k!)` bits a cohort for `k` hotkeys, read back from labels it
//! cannot see. So entrants are listed in roster order, which admission
//! fixes before any document is scored, and a cluster is named by its first
//! member in that order. A rank may return only once the quantized score
//! exists, derived from that score alone and counted in the specification's
//! per-cohort bit budget (`QUANTIZED-SCORE`). Removing the rank closes only
//! this summary's channel. The ranking of retained entrants is still public
//! outside it, until the quantization specification or a reward-rule change
//! addresses it: the coordinator's signed row (published beside its
//! authorization) and the weights revealed on chain carry credits that decay
//! by position within a band (`reward_v1::band_credits`), so they order every
//! retained cluster within its band, and the row's largest-remainder pass
//! gives a cluster's leftover units to its members in rank order; the
//! promoted entrant is the first retained head that passes promotion.
//! [`crate::allowlist`] names every field this schema may carry.

use serde::{Deserialize, Serialize};
use slop_ninja_admission_wire::json::Chain;
use slop_ninja_protocol::EntrantScore;

use crate::ss58;
use crate::verify::VerifiedCohort;

pub const SUMMARY_SCHEMA: &str = "slop_ninja.public.cohort_summary/v2";
/// The summary's name in a public cohort directory.
pub const SUMMARY: &str = "summary.json";

/// A model by its inference-content and artifact digests.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelRef {
    pub content: String,
    pub artifact: String,
}

/// `R_k` and `R_k-1` as the cohort's manifest named them.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct References {
    /// `k`.
    pub index: u64,
    pub current: ModelRef,
    pub predecessor: Option<ModelRef>,
    // QUANTIZED-SCORE: R_k's and R_k-1's quantized scores join here once the
    // specification is approved; until then no reference score is published.
}

/// An entrant's near-copy cluster, named by its first member in roster
/// order: a label every member shares, chosen before any document is scored.
/// It is not the cluster's head (its best-scoring member). Neither this
/// summary nor the artifact withholding (a cluster is withheld whole) names
/// the head, but the signed row can: a cluster's leftover units go to its
/// members in rank order (see the module documentation).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClusterRef {
    pub uid: u16,
    pub hotkey: String,
}

/// One entrant: identity, band and three bits. No rank (see the module
/// documentation).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Entrant {
    pub uid: u16,
    pub hotkey: String,
    pub hotkey_hex: String,
    // QUANTIZED-SCORE: no rank until the quantized score exists; a rank may
    // then be derived from that score alone, counted in the bit budget.
    /// Its cluster's reward band, `1` the top occupied band; `None` below
    /// the cutoff or without a target.
    pub band: Option<u32>,
    // QUANTIZED-SCORE: the entrant's quantized score joins here once the
    // specification is approved.
    /// `pass` (every enforced gate passed and it was scored), `fail`, or
    /// `not_executed` (never scored: a copy of published content, or it
    /// failed qualification).
    pub gate: String,
    /// `pass`, or `fail` when the public-copy gate refused it.
    pub copy_gate: String,
    /// Its near-copy cluster, for an entrant at or above the cutoff.
    pub cluster: Option<ClusterRef>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CohortSummary {
    pub schema: String,
    pub network: String,
    pub chain: Chain,
    pub cohort: u64,
    pub label: String,
    pub opened_epoch: u64,
    pub closed_epoch: u64,
    pub references: References,
    /// The entrant promoted to `R_k+1` (SS58), if any.
    pub promoted: Option<String>,
    /// Every entrant, in roster order.
    pub entrants: Vec<Entrant>,
}

pub fn gate_bit(e: &EntrantScore) -> &'static str {
    match (e.score.is_some(), e.gate_failures.is_empty()) {
        (false, _) => "not_executed",
        (true, true) => "pass",
        (true, false) => "fail",
    }
}

pub fn copy_bit(e: &EntrantScore) -> &'static str {
    if e.public_copy().is_some() {
        "fail"
    } else {
        "pass"
    }
}

/// The entrants of a verified cohort, in roster order (the package's
/// order, fixed at admission). Nothing here depends on an entrant's
/// position in rank order: the band is shared by a band's clusters and the
/// cluster label by its members.
pub fn entrants(v: &VerifiedCohort) -> Vec<Entrant> {
    let p = &v.package;
    let ev = &v.evaluation;
    let mut band = vec![None; p.entrants.len()];
    let mut label = vec![None; p.entrants.len()];
    for c in &ev.clusters {
        let Some(&first) = c
            .cluster
            .members
            .iter()
            .filter(|&&m| m < p.entrants.len())
            .min()
        else {
            continue;
        };
        for &m in &c.cluster.members {
            if m < p.entrants.len() {
                band[m] = c.placement.as_ref().map(|pl| pl.band_rank as u32 + 1);
                label[m] = Some(first);
            }
        }
    }
    p.entrants
        .iter()
        .enumerate()
        .map(|(i, e)| Entrant {
            uid: e.identity.uid,
            hotkey: ss58::encode(&e.identity.hotkey),
            hotkey_hex: hex::encode(e.identity.hotkey.0),
            band: band[i],
            gate: gate_bit(e).into(),
            copy_gate: copy_bit(e).into(),
            cluster: label[i].map(|f| {
                let f = &p.entrants[f];
                ClusterRef {
                    uid: f.identity.uid,
                    hotkey: ss58::encode(&f.identity.hotkey),
                }
            }),
        })
        .collect()
}

/// The summary of a verified cohort.
pub fn build(v: &VerifiedCohort, chain: Chain) -> CohortSummary {
    let refs = &v.manifest.references;
    let model = |m: &slop_ninja_protocol::ReferenceModel| ModelRef {
        content: m.content.hex(),
        artifact: m.artifact.hex(),
    };
    CohortSummary {
        schema: SUMMARY_SCHEMA.into(),
        network: "testnet".into(),
        chain,
        cohort: v.manifest.cohort,
        label: v.label.clone(),
        opened_epoch: v.manifest.opened_epoch,
        closed_epoch: v.closure.closed_epoch,
        references: References {
            index: refs.index,
            current: model(&refs.current),
            predecessor: refs.predecessor.as_ref().map(model),
        },
        promoted: v.package.promoted.map(|h| ss58::encode(&h)),
        entrants: entrants(v),
    }
}
