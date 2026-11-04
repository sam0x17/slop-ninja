//! The public summary of one closed cohort
//! (`v1/cohorts/<label>/summary.json`, schema
//! `slop_ninja.public.cohort_summary/v1`): the only cohort object the public
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
//! - per entrant: its UID and hotkey, its rank among gated entrants, its
//!   reward band (`1` is the top occupied band), one gate bit
//!   (`pass`, `fail`, or `not_executed` for an entrant never scored), one
//!   public-copy gate bit and its near-copy cluster membership (the head's
//!   hotkey and UID);
//! - `R_k` and `R_k-1` as the cohort's manifest named them, by content and
//!   artifact digest, and the entrant promoted to `R_k+1`, if any;
//! - the cohort's number, label and opening and closing epochs.
//!
//! No score value is published, not even rounded (the user's quantization
//! specification is pending). Every place that will carry the quantized
//! value once it is approved is marked `QUANTIZED-SCORE`; until then a rank
//! and a band are all a reader learns about a model's measured quality.
//! [`crate::allowlist`] names every field this schema may carry.

use serde::{Deserialize, Serialize};
use slop_ninja_admission_wire::json::Chain;
use slop_ninja_protocol::{EntrantScore, ScorePackageV2, reward_v1};

use crate::ss58;
use crate::verify::VerifiedCohort;

pub const SUMMARY_SCHEMA: &str = "slop_ninja.public.cohort_summary/v1";
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

/// The head of an entrant's near-copy cluster.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClusterRef {
    pub head: String,
    pub head_uid: u16,
}

/// One entrant: identity, rank, band and three bits.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Entrant {
    pub uid: u16,
    pub hotkey: String,
    pub hotkey_hex: String,
    /// Position among gated entrants, from 1; `None` for an entrant that
    /// failed a gate or was never scored.
    pub rank: Option<u32>,
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
    /// Its near-copy cluster's head, for an entrant at or above the cutoff.
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
    /// Rank order first, then the other entrants in roster order.
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

/// Every entrant in summary order: rank order, then the rest in roster
/// order. Returns `(package index, rank)`.
pub fn order(p: &ScorePackageV2) -> Vec<(usize, Option<u32>)> {
    let ranked = reward_v1::rank_order(p);
    let mut out: Vec<(usize, Option<u32>)> = ranked
        .iter()
        .enumerate()
        .map(|(pos, &i)| (i, Some(pos as u32 + 1)))
        .collect();
    out.extend(
        (0..p.entrants.len())
            .filter(|i| !ranked.contains(i))
            .map(|i| (i, None)),
    );
    out
}

/// The entrants of a verified cohort, in summary order.
pub fn entrants(v: &VerifiedCohort) -> Vec<Entrant> {
    let p = &v.package;
    let ev = &v.evaluation;
    let mut band = vec![None; p.entrants.len()];
    let mut head = vec![None; p.entrants.len()];
    for c in &ev.clusters {
        for &m in &c.cluster.members {
            if m < p.entrants.len() {
                band[m] = c.placement.as_ref().map(|pl| pl.band_rank as u32 + 1);
                head[m] = Some(c.cluster.head);
            }
        }
    }
    order(p)
        .into_iter()
        .map(|(i, rank)| {
            let e = &p.entrants[i];
            Entrant {
                uid: e.identity.uid,
                hotkey: ss58::encode(&e.identity.hotkey),
                hotkey_hex: hex::encode(e.identity.hotkey.0),
                rank,
                band: band[i],
                gate: gate_bit(e).into(),
                copy_gate: copy_bit(e).into(),
                cluster: head[i].map(|h| {
                    let h = &p.entrants[h];
                    ClusterRef {
                        head: ss58::encode(&h.identity.hotkey),
                        head_uid: h.identity.uid,
                    }
                }),
            }
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
