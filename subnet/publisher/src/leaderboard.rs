//! The leaderboard: `v1/leaderboard.json` and the static `index.html` page
//! rendered from it at publication time (template and stylesheet in `site/`).
//!
//! Every value comes from the verified signed package and the outcome the
//! publisher recomputed from it ([`reward_v1::check_outcome`]): hotkey and
//! UID, the Brier utility, gate failures, the public-copy outcome, the
//! near-copy cluster, the band, the credit and units, plus the active target
//! and the reference. Nothing per document appears.

use serde::{Deserialize, Serialize};
use slop_ninja_admission_wire::json::Chain;
use slop_ninja_protocol::bigint::Fraction;
use slop_ninja_protocol::reward_v1;
use slop_ninja_protocol::{EntrantScore, ROW_TOTAL, SCORE_SCALE, Score};

use crate::index::{ActiveTarget, ArtifactEntry, ReferencePointer, mode_name};
use crate::ss58;
use crate::verify::VerifiedCohort;

pub const LEADERBOARD_SCHEMA: &str = "slop_ninja.public.leaderboard/v1";
pub const TEMPLATE: &str = include_str!("../../../site/index.html");
pub const STYLE: &str = include_str!("../../../site/style.css");

/// A score in `1 / SCORE_SCALE` units as a decimal with six places (floor).
pub fn score_decimal(raw: u128) -> String {
    let scale = u128::from(SCORE_SCALE);
    let whole = raw / scale;
    let frac = (raw % scale) / (scale / 1_000_000);
    format!("{whole}.{frac:06}")
}

fn fraction_decimal(f: &Fraction) -> String {
    let scaled = f.floor_scaled(1_000_000_000).to_u128().unwrap_or(0);
    format!("{}.{:09}", scaled / 1_000_000_000, scaled % 1_000_000_000)
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScoreView {
    pub decimal: String,
    /// Exact, in units of 10^-18.
    pub raw: String,
}

fn score_view(s: Score) -> ScoreView {
    ScoreView {
        decimal: score_decimal(u128::from(s.get())),
        raw: s.get().to_string(),
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CopyView {
    /// `independent`, `not_executed`, or a public-copy reason code.
    pub outcome: String,
    /// Aligned distance to `R_k` on the batch, in parts per billion.
    pub distance_to_reference_ppb: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClusterView {
    pub head: String,
    pub head_uid: u16,
    /// `head`, `identical` (same inference content) or `near` (by distance).
    pub joined: String,
    pub distance_ppb: Option<u32>,
    pub members: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BandView {
    /// Absolute band index `b`.
    pub band: String,
    /// `kappa`, from the top occupied band.
    pub band_rank: u32,
    /// `mu`, within the band.
    pub rank_in_band: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreditView {
    /// The cluster's whole credit `c`, exact.
    pub cluster: String,
    /// This member's share `c / m`, exact and as a decimal.
    pub member: String,
    pub decimal: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EntrantRow {
    /// Position in rank order among gated entrants, from 1.
    pub rank: Option<u32>,
    pub uid: u16,
    pub hotkey: String,
    pub hotkey_hex: String,
    pub score: Option<ScoreView>,
    pub assigned: u32,
    pub succeeded: u32,
    pub failed: u32,
    pub gates: Vec<String>,
    pub copy: CopyView,
    pub cluster: Option<ClusterView>,
    pub band: Option<BandView>,
    pub credit: Option<CreditView>,
    pub units: u16,
    /// Units over the row total, as a decimal.
    pub row_share: String,
    pub artifact: Option<ArtifactEntry>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReferenceView {
    pub index: u64,
    pub content: String,
    pub score: ScoreView,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LeaderboardDoc {
    pub schema: String,
    pub network: String,
    pub chain: Chain,
    pub cohort: u64,
    pub label: String,
    pub closed_epoch: u64,
    pub mode: String,
    /// The cutoff in score units, when the cohort had one.
    pub threshold: Option<String>,
    pub band_width: Option<String>,
    pub reference: ReferenceView,
    pub predecessor: Option<ReferenceView>,
    pub promoted: Option<String>,
    pub active_target: ActiveTarget,
    /// `R_k` and `R_k-1` after this cohort's close.
    pub current_reference: ReferencePointer,
    pub previous_reference: Option<ReferencePointer>,
    /// Rank order first, then gate failures in roster order.
    pub entrants: Vec<EntrantRow>,
}

fn copy_view(e: &EntrantScore) -> CopyView {
    CopyView {
        outcome: match (e.public_copy(), e.score) {
            (Some(c), _) => c.reason().to_owned(),
            (None, Some(_)) => "independent".into(),
            (None, None) => "not_executed".into(),
        },
        distance_to_reference_ppb: e.public.as_ref().and_then(|p| p.current.distance_ppb()),
    }
}

pub struct Inputs<'a> {
    pub cohort: &'a VerifiedCohort,
    pub artifacts: &'a [ArtifactEntry],
    pub active_target: &'a ActiveTarget,
    pub current_reference: ReferencePointer,
    pub previous_reference: Option<ReferencePointer>,
    pub chain: Chain,
}

/// Builds the leaderboard of one verified cohort.
pub fn build(i: Inputs<'_>) -> LeaderboardDoc {
    let v = i.cohort;
    let p = &v.package;
    let ev = &v.evaluation;
    let n = p.entrants.len();
    // Per entrant: (cluster index, member count, placement).
    let mut cluster_of = vec![None; n];
    for (ci, outcome) in ev.clusters.iter().enumerate() {
        for &m in &outcome.cluster.members {
            if m < n {
                cluster_of[m] = Some(ci);
            }
        }
    }
    let ranked = reward_v1::rank_order(p);
    let mut rank = vec![None; n];
    for (pos, &idx) in ranked.iter().enumerate() {
        if idx < n {
            rank[idx] = Some(pos as u32 + 1);
        }
    }
    let row = |idx: usize| {
        let e = &p.entrants[idx];
        let cluster = cluster_of[idx].map(|ci| &ev.clusters[ci]);
        let head = cluster.map(|c| &p.entrants[c.cluster.head]);
        let members = cluster.map_or(0, |c| c.cluster.members.len() as u32);
        let placement = cluster.and_then(|c| c.placement.as_ref());
        EntrantRow {
            rank: rank[idx],
            uid: e.identity.uid,
            hotkey: ss58::encode(&e.identity.hotkey),
            hotkey_hex: hex::encode(e.identity.hotkey.0),
            score: e.score.map(score_view),
            assigned: e.assigned,
            succeeded: e.succeeded,
            failed: e.failed,
            gates: e.gate_failures.iter().map(|g| g.reason.clone()).collect(),
            copy: copy_view(e),
            cluster: head.map(|h| ClusterView {
                head: ss58::encode(&h.identity.hotkey),
                head_uid: h.identity.uid,
                joined: if h.identity.hotkey == e.identity.hotkey {
                    "head"
                } else if e.cluster_stats.is_some() {
                    "near"
                } else {
                    "identical"
                }
                .into(),
                distance_ppb: e.cluster_stats.and_then(|s| s.distance_ppb()),
                members,
            }),
            band: placement.map(|pl| BandView {
                band: pl.band.to_string(),
                band_rank: pl.band_rank as u32,
                rank_in_band: pl.rank_in_band as u32,
            }),
            credit: placement.and_then(|pl| {
                let m = slop_ninja_protocol::bigint::BigUint::from_u128(u128::from(members.max(1)));
                let member = Fraction::new(pl.credit.num.clone(), pl.credit.den.mul(&m))?.reduced();
                Some(CreditView {
                    cluster: format!("{}/{}", pl.credit.num, pl.credit.den),
                    member: format!("{}/{}", member.num, member.den),
                    decimal: fraction_decimal(&member),
                })
            }),
            units: e.units,
            row_share: {
                let scaled = u64::from(e.units) * 1_000_000 / u64::from(ROW_TOTAL);
                format!("{}.{:06}", scaled / 1_000_000, scaled % 1_000_000)
            },
            artifact: i
                .artifacts
                .iter()
                .find(|a| {
                    a.hotkey == ss58::encode(&e.identity.hotkey) && a.sha256 == e.artifact.hex()
                })
                .cloned(),
        }
    };
    let mut order: Vec<usize> = ranked.clone();
    order.extend((0..n).filter(|idx| !ranked.contains(idx)));
    LeaderboardDoc {
        schema: LEADERBOARD_SCHEMA.into(),
        network: "testnet".into(),
        chain: i.chain,
        cohort: v.manifest.cohort,
        label: v.label.clone(),
        closed_epoch: v.closure.closed_epoch,
        mode: mode_name(p.mode).into(),
        threshold: ev
            .threshold
            .as_ref()
            .map(|t| score_decimal(t.num / t.den.max(1))),
        band_width: ev.bands.map(|b| fraction_decimal(&b.width_fraction())),
        reference: ReferenceView {
            index: p.reference_index,
            content: p.reference.model.content.hex(),
            score: score_view(p.reference.score),
        },
        predecessor: p.predecessor.as_ref().map(|r| ReferenceView {
            index: p.reference_index.saturating_sub(1),
            content: r.model.content.hex(),
            score: score_view(r.score),
        }),
        promoted: p.promoted.map(|h| ss58::encode(&h)),
        active_target: i.active_target.clone(),
        current_reference: i.current_reference,
        previous_reference: i.previous_reference,
        entrants: order.into_iter().map(row).collect(),
    }
}

/// HTML-escapes text for element content and attribute values.
pub fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

fn short(hex: &str) -> String {
    if hex.len() > 16 {
        format!("{}…{}", &hex[..8], &hex[hex.len() - 6..])
    } else {
        hex.to_owned()
    }
}

/// Renders the static page. `doc` is `None` before the first cohort closes.
pub fn render(doc: Option<&LeaderboardDoc>) -> String {
    let (summary, rows) = match doc {
        None => (
            "<p class=\"empty\">No cohort of the public record has closed yet.</p>".to_owned(),
            String::new(),
        ),
        Some(d) => {
            let mut summary = format!(
                "<dl class=\"facts\">\
                 <div><dt>Cohort</dt><dd>{} <span class=\"muted\">{}</span></dd></div>\
                 <div><dt>Closed at epoch</dt><dd>{}</dd></div>\
                 <div><dt>Outcome</dt><dd>{}</dd></div>\
                 <div><dt>Reference R<sub>{}</sub> score</dt><dd class=\"num\">{}</dd></div>",
                d.cohort,
                escape(&d.label),
                d.closed_epoch,
                escape(&d.mode),
                d.reference.index,
                escape(&d.reference.score.decimal),
            );
            if let Some(t) = &d.threshold {
                summary.push_str(&format!(
                    "<div><dt>Cutoff</dt><dd class=\"num\">{}</dd></div>",
                    escape(t)
                ));
            }
            summary.push_str(&format!(
                "<div><dt>Active target</dt><dd><code title=\"{}\">{}</code> <span class=\"muted\">{}</span></dd></div>",
                escape(&d.active_target.digest),
                escape(&short(&d.active_target.digest)),
                escape(&match d.active_target.cohort {
                    Some(c) => format!("cohort {c}"),
                    None => "launch burn".to_owned(),
                }),
            ));
            let r = &d.current_reference;
            summary.push_str(&format!(
                "<div><dt>Current reference R<sub>{}</sub></dt><dd><code title=\"{}\">{}</code> {}</dd></div></dl>",
                r.index,
                escape(&r.content),
                escape(&short(&r.content)),
                match (&r.path, &r.package) {
                    (Some(p), _) => format!("<a href=\"{}\">download</a>", escape(p)),
                    (None, Some(dir)) => format!(
                        "<a href=\"{}package.json\">reference package</a> <span class=\"muted\">{}</span>",
                        escape(dir),
                        escape(r.licence.as_deref().unwrap_or_default()),
                    ),
                    (None, None) => format!("<span class=\"muted\">{}</span>", crate::index::REFERENCE_NOT_STAGED),
                }
            ));
            let rows: String = d
                .entrants
                .iter()
                .map(|e| {
                    let artifact = match &e.artifact {
                        Some(a) => match (&a.path, a.state) {
                            (Some(p), crate::index::ArtifactState::Published) => {
                                format!("<a href=\"{}\">artifact</a>", escape(p))
                            }
                            (_, s) => format!("<span class=\"muted\">{}</span>", escape(&format!("{s:?}").to_lowercase())),
                        },
                        None => String::new(),
                    };
                    format!(
                        "<tr><td class=\"num\">{}</td><td class=\"num\">{}</td><td><code title=\"{}\">{}</code></td>\
                         <td class=\"num\">{}</td><td>{}</td><td>{}</td><td>{}</td><td class=\"num\">{}</td>\
                         <td class=\"num\">{}</td><td class=\"num\">{}</td><td>{}</td></tr>",
                        e.rank.map(|r| r.to_string()).unwrap_or_else(|| "–".into()),
                        e.uid,
                        escape(&e.hotkey),
                        escape(&short(&e.hotkey)),
                        e.score.as_ref().map(|s| escape(&s.decimal)).unwrap_or_else(|| "–".into()),
                        if e.gates.is_empty() { "pass".to_owned() } else { escape(&e.gates.join(", ")) },
                        escape(&e.copy.outcome),
                        e.cluster
                            .as_ref()
                            .map(|c| format!("UID {} ({})", c.head_uid, escape(&c.joined)))
                            .unwrap_or_else(|| "–".into()),
                        e.band.as_ref().map(|b| format!("{} / κ{}", escape(&b.band), b.band_rank)).unwrap_or_else(|| "–".into()),
                        e.credit.as_ref().map(|c| escape(&c.decimal)).unwrap_or_else(|| "–".into()),
                        e.units,
                        artifact,
                    )
                })
                .collect();
            (summary, rows)
        }
    };
    TEMPLATE
        .replace("<!--SUMMARY-->", &summary)
        .replace("<!--ROWS-->", &rows)
}
