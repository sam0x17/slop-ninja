//! The public summary of one closed cohort
//! (`v1/cohorts/<label>/summary.json`, schema
//! `slop_ninja.public.cohort_summary/v3`): the only cohort object the public
//! host carries.
//!
//! The coordinator keeps every record computed from the hidden evaluation
//! private: the signed manifest's strata and source groups, the score
//! package (exact scores, near-copy statistics, probe matches, the distance
//! monitor, promotion audits, attempt counts, the SE sums and the
//! quantization draws, the receipt root and count), the target, the
//! published-content and fingerprint indexes, the components and every
//! receipt. What it publishes is the signed `QuantizedResult` (`quant-v1`),
//! which the publisher verifies in the private bucket
//! ([`crate::verify`]) and from which alone this summary is built:
//!
//! - the grid, as `quantization {rule, width}`; every score below is a
//!   multiple of `width`, printed with exactly its decimals;
//! - per entrant, in roster (admission) order: its UID and hotkey, its
//!   quantized score `q`, its reward band (`1` is the top occupied band),
//!   one gate bit (`pass`, `fail`, `withheld` for an entrant the
//!   minimum-success gate withheld, or `not_executed` for an entrant never
//!   scored), one public-copy gate bit and its near-copy cluster membership
//!   (named by the cluster's first member in roster order). A withheld
//!   entrant publishes its identity and nothing else;
//! - `R_k` and `R_k-1` by content and artifact digest with their `q`, and
//!   the entrant promoted to `R_k+1`, if any;
//! - `quality`: the cutoff on the grid with its mode, and the occupied
//!   bands' absolute grid edges;
//! - the cohort's number, label and opening and closing epochs.
//!
//! No exact score, SE, offset, noise or statistic is published, and no
//! rank: entrants are listed in roster order, which admission fixes before
//! any document is scored, and a cluster is named by its first member in
//! that order. The q-rank (`q` descending, then roster order) follows from
//! the published values and adds no bits beyond them; it is also the order
//! rewards are computed in (`reward-v2`), and every credit, band and unit
//! is a function of the published `q` values, bits and membership alone.
//! [`crate::allowlist`] names every field this schema may carry.

use serde::{Deserialize, Serialize};
use slop_ninja_admission_wire::json::Chain;
use slop_ninja_protocol::quant::{self, MAX_DECIMAL_LEN};
use slop_ninja_protocol::{CutoffMode, EntrantOutcome, NotExecuted, QuantizedResult, ResultReference};

use crate::ss58;
use crate::verify::VerifiedCohort;

pub const SUMMARY_SCHEMA: &str = "slop_ninja.public.cohort_summary/v3";
/// The summary's name in a public cohort directory.
pub const SUMMARY: &str = "summary.json";

/// How the scores of a document were published: the quantization rule and
/// the grid width `w`, a decimal string. Every score is a multiple of `w`
/// printed with exactly `w`'s decimals.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Quantization {
    pub rule: String,
    pub width: String,
}

/// A model by its inference-content and artifact digests, with its
/// quantized score on this cohort's batch. Every v3 summary carries `q`;
/// `None` only in a summary listed before quantization ([`read`]).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelRef {
    pub content: String,
    pub artifact: String,
    pub q: Option<String>,
}

/// `R_k` and `R_k-1` as the cohort's manifest named them.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct References {
    /// `k`.
    pub index: u64,
    pub current: ModelRef,
    pub predecessor: Option<ModelRef>,
}

/// An entrant's near-copy cluster, named by its first member in roster
/// order: a label every member shares, chosen before any document is
/// scored. It is not the cluster's head (its first member in q-rank).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClusterRef {
    pub uid: u16,
    pub hotkey: String,
}

/// One entrant: identity, `q`, band and three bits. No rank (see the module
/// documentation).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Entrant {
    pub uid: u16,
    pub hotkey: String,
    pub hotkey_hex: String,
    /// The quantized score, a multiple of the summary's width; `None` for an
    /// entrant withheld or never executed.
    pub q: Option<String>,
    /// Its cluster's reward band, `1` the top occupied band; `None` below
    /// the cutoff or without a target.
    pub band: Option<u32>,
    /// `pass` (every gate passed, the public-copy gate included), `fail`,
    /// `withheld` (the minimum-success gate: nothing else is published) or
    /// `not_executed` (never scored: a copy of published content, or it
    /// failed qualification).
    pub gate: String,
    /// `pass`, or `fail` when the public-copy gate refused it; `None` for a
    /// withheld entrant.
    pub copy_gate: Option<String>,
    /// Its near-copy cluster, for an entrant at or above the cutoff.
    pub cluster: Option<ClusterRef>,
}

/// The cutoff on the grid and the rule that set it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Cutoff {
    pub q: String,
    /// `margin` (an entrant beat `R_k` by the margin) or `fallback` (`R_k-1`
    /// plus 15% of the cohort's range).
    pub mode: String,
}

/// One occupied band, in absolute grid units: `[lower, upper)`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Band {
    /// `1` the top occupied band.
    pub band: u32,
    pub lower: String,
    pub upper: String,
}

/// The cohort's cutoff and occupied bands; `cutoff` is `None` and `bands`
/// empty for a cohort without a target.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Quality {
    pub cutoff: Option<Cutoff>,
    pub bands: Vec<Band>,
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
    /// Every v3 summary carries the block; `None` only for a summary listed
    /// before quantization ([`read`]), which carries no score at all.
    pub quantization: Option<Quantization>,
    pub references: References,
    pub quality: Quality,
    /// The entrant promoted to `R_k+1` (SS58), if any.
    pub promoted: Option<String>,
    /// Every entrant, in roster order.
    pub entrants: Vec<Entrant>,
}

/// `value` printed with exactly `w`'s decimals: a `q`, a cutoff or a band's
/// lower edge (each a multiple of `w`, [`quant::format_grid`]), or a band's
/// upper edge, which is a multiple of `w` or the score scale's `1`. A verified
/// result carries no other value, so the empty string (a value with more
/// decimals than `w`) never reaches a document: the allow-list and the
/// dashboard's grid check refuse it.
pub fn decimal(value: u64, w: u64) -> String {
    let mut buf = [0u8; MAX_DECIMAL_LEN];
    if let Ok(text) = quant::format_grid(value, w, &mut buf) {
        return text.to_owned();
    }
    match quant::decimals(w) {
        Ok(d) if value == slop_ninja_protocol::SCORE_SCALE => {
            let mut one = String::from("1");
            if d > 0 {
                one.push('.');
                one.extend(std::iter::repeat_n('0', d as usize));
            }
            one
        }
        _ => String::new(),
    }
}

/// The quantization block of a result.
pub fn quantization(r: &QuantizedResult) -> Quantization {
    Quantization {
        rule: r.quant_rule.name().into(),
        width: decimal(r.width, r.width),
    }
}

pub fn gate_bit(outcome: &EntrantOutcome) -> &'static str {
    match outcome {
        EntrantOutcome::NotExecuted(_) => "not_executed",
        EntrantOutcome::Withheld => "withheld",
        EntrantOutcome::Scored {
            gate_pass: true,
            copy_pass: true,
            ..
        } => "pass",
        EntrantOutcome::Scored { .. } => "fail",
    }
}

/// The public-copy bit; `None` for a withheld entrant.
pub fn copy_bit(outcome: &EntrantOutcome) -> Option<&'static str> {
    match outcome {
        EntrantOutcome::NotExecuted(NotExecuted::Content) => Some("fail"),
        EntrantOutcome::NotExecuted(NotExecuted::Qualification) => Some("pass"),
        EntrantOutcome::Withheld => None,
        EntrantOutcome::Scored { copy_pass, .. } => Some(if *copy_pass { "pass" } else { "fail" }),
    }
}

/// Whether the public-copy gate refused the entrant (a copy of published
/// content or of a public artifact).
pub fn is_copy(outcome: &EntrantOutcome) -> bool {
    copy_bit(outcome) == Some("fail")
}

/// The entrants of a verified cohort, in roster order (the result's order,
/// fixed at admission).
pub fn entrants(v: &VerifiedCohort) -> Vec<Entrant> {
    let r = &v.result;
    r.entrants
        .iter()
        .map(|e| {
            let cluster = match e.outcome {
                EntrantOutcome::Scored {
                    cluster_first_member: Some(f),
                    ..
                } => r.entrants.get(usize::from(f)).map(|f| ClusterRef {
                    uid: f.uid,
                    hotkey: ss58::encode(&f.hotkey),
                }),
                _ => None,
            };
            Entrant {
                uid: e.uid,
                hotkey: ss58::encode(&e.hotkey),
                hotkey_hex: hex::encode(e.hotkey.0),
                q: e.outcome.q().map(|q| decimal(q.get(), r.width)),
                band: e.band_rank.map(|b| u32::from(b) + 1),
                gate: gate_bit(&e.outcome).into(),
                copy_gate: copy_bit(&e.outcome).map(Into::into),
                cluster,
            }
        })
        .collect()
}

fn model(m: &ResultReference, w: u64) -> ModelRef {
    ModelRef {
        content: m.content.hex(),
        artifact: m.artifact.hex(),
        q: Some(decimal(m.q.get(), w)),
    }
}

/// The cutoff and the occupied bands of a result.
pub fn quality(r: &QuantizedResult) -> Quality {
    let w = r.width;
    let mode = match r.mode {
        CutoffMode::Margin => Some("margin"),
        CutoffMode::Fallback => Some("fallback"),
        CutoffMode::NoTarget => None,
    };
    Quality {
        cutoff: r.cutoff.zip(mode).map(|(c, mode)| Cutoff {
            q: decimal(c.get(), w),
            mode: mode.into(),
        }),
        bands: r
            .bands
            .iter()
            .map(|b| Band {
                band: u32::from(b.rank) + 1,
                lower: decimal(b.lower.get(), w),
                upper: decimal(b.upper.get(), w),
            })
            .collect(),
    }
}

/// The summary of a verified cohort.
pub fn build(v: &VerifiedCohort, chain: Chain) -> CohortSummary {
    let r = &v.result;
    CohortSummary {
        schema: SUMMARY_SCHEMA.into(),
        network: "testnet".into(),
        chain,
        cohort: r.cohort,
        label: v.label.clone(),
        opened_epoch: r.opened_epoch,
        closed_epoch: v.closure.closed_epoch,
        quantization: Some(quantization(r)),
        references: References {
            index: r.reference_index,
            current: model(&r.current, r.width),
            predecessor: r.predecessor.as_ref().map(|p| model(p, r.width)),
        },
        quality: quality(r),
        promoted: r.promoted.map(|h| ss58::encode(&h)),
        entrants: entrants(v),
    }
}

/// The summary schema listed before quantization (cohorts closed under
/// `reward-v1`): no score, no cutoff and no band edges.
pub const SUMMARY_SCHEMA_V2: &str = "slop_ninja.public.cohort_summary/v2";

/// The frozen `cohort_summary/v2` shape, read only: a summary is never
/// rewritten once listed (`docs/testnet/public-layout.md`), so cohorts
/// listed before quantization keep their v2 document.
pub mod v2 {
    use serde::Deserialize;
    use slop_ninja_admission_wire::json::Chain;

    #[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
    #[serde(deny_unknown_fields)]
    pub struct ModelRef {
        pub content: String,
        pub artifact: String,
    }

    #[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
    #[serde(deny_unknown_fields)]
    pub struct References {
        pub index: u64,
        pub current: ModelRef,
        pub predecessor: Option<ModelRef>,
    }

    #[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
    #[serde(deny_unknown_fields)]
    pub struct Entrant {
        pub uid: u16,
        pub hotkey: String,
        pub hotkey_hex: String,
        pub band: Option<u32>,
        pub gate: String,
        pub copy_gate: String,
        pub cluster: Option<super::ClusterRef>,
    }

    #[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
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
        pub promoted: Option<String>,
        pub entrants: Vec<Entrant>,
    }
}

/// A listed summary as readers take it: a v3 document as is, or a v2
/// document (listed before quantization) as a summary with no quantization
/// block, no `q`, no cutoff and no band edges, its band numbers and bits
/// kept. `None` for anything else.
pub fn read(bytes: &[u8]) -> Option<CohortSummary> {
    let schema = serde_json::from_slice::<serde_json::Value>(bytes).ok()?;
    match schema.get("schema").and_then(|s| s.as_str())? {
        SUMMARY_SCHEMA => serde_json::from_slice(bytes).ok(),
        SUMMARY_SCHEMA_V2 => {
            let old: v2::CohortSummary = serde_json::from_slice(bytes).ok()?;
            let model = |m: v2::ModelRef| ModelRef {
                content: m.content,
                artifact: m.artifact,
                q: None,
            };
            Some(CohortSummary {
                schema: old.schema,
                network: old.network,
                chain: old.chain,
                cohort: old.cohort,
                label: old.label,
                opened_epoch: old.opened_epoch,
                closed_epoch: old.closed_epoch,
                quantization: None,
                references: References {
                    index: old.references.index,
                    current: model(old.references.current),
                    predecessor: old.references.predecessor.map(model),
                },
                quality: Quality {
                    cutoff: None,
                    bands: Vec::new(),
                },
                promoted: old.promoted,
                entrants: old
                    .entrants
                    .into_iter()
                    .map(|e| Entrant {
                        uid: e.uid,
                        hotkey: e.hotkey,
                        hotkey_hex: e.hotkey_hex,
                        q: None,
                        band: e.band,
                        gate: e.gate,
                        copy_gate: Some(e.copy_gate),
                        cluster: e.cluster,
                    })
                    .collect(),
            })
        }
        _ => None,
    }
}
