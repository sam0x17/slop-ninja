//! The leaderboard: `v1/leaderboard.json` and the static `index.html` page
//! rendered from it at publication time (template and stylesheet in `site/`).
//!
//! Schema v3 carries the newest cohort's allow-listed summary
//! ([`crate::summary`]) and nothing else computed from the hidden
//! evaluation: per entrant, in roster order, its UID and hotkey, band, gate
//! and public-copy gate bits, cluster membership and artifact state, plus
//! the active target and the reference pointers. No score, rank, threshold,
//! band width, credit, count or distance appears, and the rows are not in
//! rank order (schema v2 carried a rank and listed rows by it); every place
//! that will carry the quantized score once its specification is approved
//! is marked `QUANTIZED-SCORE`.

use serde::{Deserialize, Serialize};
use slop_ninja_admission_wire::json::Chain;

use crate::index::{ActiveTarget, ArtifactEntry, ReferencePointer};
use crate::summary::{self, CohortSummary};

pub const LEADERBOARD_SCHEMA: &str = "slop_ninja.public.leaderboard/v3";
pub const TEMPLATE: &str = include_str!("../../../site/index.html");
pub const STYLE: &str = include_str!("../../../site/style.css");

/// One leaderboard row: the summary's entrant and its artifact's state.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EntrantRow {
    #[serde(flatten)]
    pub entrant: summary::Entrant,
    pub artifact: Option<ArtifactEntry>,
}

/// How promotions of this record are decided (user decision D12b,
/// 2026-11-03): testnet promotions run without the lookup canary; the canary
/// is required before mainnet. A label only: nothing else changes with it,
/// so an operator drill in the plan (`canary.drill`) can still flag and
/// block a testnet candidate, as the note says.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromotionRule {
    /// Whether promotions run the lookup canary.
    pub lookup_canary: bool,
    /// `testnet-only`: the rule holds for the testnet record only.
    pub scope: String,
    pub note: String,
}

/// The note the leaderboard JSON and page carry.
pub const PROMOTION_NOTE: &str = "Testnet only: promotions in this record run without the lookup canary. \
    An operator drill of the blocking path, named in the cohort's plan, can still flag a candidate and \
    block its promotion. The canary is required before mainnet.";

impl PromotionRule {
    pub fn testnet() -> Self {
        Self {
            lookup_canary: false,
            scope: "testnet-only".into(),
            note: PROMOTION_NOTE.into(),
        }
    }

    /// The rule of a record bound to `chain`: the testnet label for the
    /// testnet genesis only. A record of any other chain carries no label
    /// (the canary is required before mainnet, so no other chain's record
    /// may claim promotions without it).
    pub fn of(chain: &Chain) -> Option<Self> {
        (chain
            .genesis
            .trim_start_matches("0x")
            .eq_ignore_ascii_case(&slop_ninja_protocol::TESTNET_GENESIS.hex()))
        .then(Self::testnet)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LeaderboardDoc {
    pub schema: String,
    pub network: String,
    pub chain: Chain,
    pub cohort: u64,
    pub label: String,
    pub closed_epoch: u64,
    /// `R_k` and `R_k-1` as the cohort's manifest named them.
    pub references: summary::References,
    pub promoted: Option<String>,
    /// The promotion rule of this record (D12b): present for a testnet
    /// record only ([`PromotionRule::of`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub promotion_rule: Option<PromotionRule>,
    pub active_target: ActiveTarget,
    /// `R_k` and `R_k-1` after this cohort's close.
    pub current_reference: ReferencePointer,
    pub previous_reference: Option<ReferencePointer>,
    /// Every entrant, in roster order (the summary's order).
    pub entrants: Vec<EntrantRow>,
}

pub struct Inputs<'a> {
    pub summary: &'a CohortSummary,
    pub artifacts: &'a [ArtifactEntry],
    pub active_target: &'a ActiveTarget,
    pub current_reference: ReferencePointer,
    pub previous_reference: Option<ReferencePointer>,
}

/// Builds the leaderboard of one cohort from its summary.
pub fn build(i: Inputs<'_>) -> LeaderboardDoc {
    let s = i.summary;
    LeaderboardDoc {
        schema: LEADERBOARD_SCHEMA.into(),
        network: s.network.clone(),
        chain: s.chain.clone(),
        cohort: s.cohort,
        label: s.label.clone(),
        closed_epoch: s.closed_epoch,
        references: s.references.clone(),
        promoted: s.promoted.clone(),
        promotion_rule: PromotionRule::of(&s.chain),
        active_target: i.active_target.clone(),
        current_reference: i.current_reference,
        previous_reference: i.previous_reference,
        entrants: s
            .entrants
            .iter()
            .map(|e| EntrantRow {
                entrant: e.clone(),
                artifact: i.artifacts.iter().find(|a| a.hotkey == e.hotkey).cloned(),
            })
            .collect(),
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
                 <div><dt>Promoted</dt><dd>{}</dd></div>",
                d.cohort,
                escape(&d.label),
                d.closed_epoch,
                d.promoted
                    .as_deref()
                    .map(|p| format!("<code title=\"{}\">{}</code>", escape(p), escape(&short(p))))
                    .unwrap_or_else(|| "none".into()),
            );
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
                .map(|row| {
                    let e = &row.entrant;
                    let artifact = match &row.artifact {
                        Some(a) => match (&a.path, a.state) {
                            (Some(p), crate::index::ArtifactState::Published) => {
                                format!("<a href=\"{}\">artifact</a>", escape(p))
                            }
                            (_, s) => format!("<span class=\"muted\">{}</span>", escape(&format!("{s:?}").to_lowercase())),
                        },
                        None => String::new(),
                    };
                    // QUANTIZED-SCORE: a score column (and a rank derived
                    // from it alone) joins once the specification is
                    // approved.
                    format!(
                        "<tr><td class=\"num\">{}</td><td><code title=\"{}\">{}</code></td>\
                         <td>{}</td><td>{}</td><td>{}</td><td class=\"num\">{}</td><td>{}</td></tr>",
                        e.uid,
                        escape(&e.hotkey),
                        escape(&short(&e.hotkey)),
                        escape(&e.gate),
                        escape(&e.copy_gate),
                        e.cluster.as_ref().map(|c| format!("UID {}", c.uid)).unwrap_or_else(|| "–".into()),
                        e.band.map(|b| b.to_string()).unwrap_or_else(|| "–".into()),
                        artifact,
                    )
                })
                .collect();
            (summary, rows)
        }
    };
    // The D12b label of a testnet record (before any closed cohort, the
    // publisher's own configuration is testnet-only: `config::parse`).
    let note = match doc {
        None => Some(PROMOTION_NOTE),
        Some(d) => d.promotion_rule.as_ref().map(|r| r.note.as_str()),
    };
    let summary = match note {
        Some(n) => format!(
            "{summary}<p class=\"note\" id=\"promotion-rule\">{}</p>",
            escape(n)
        ),
        None => summary,
    };
    TEMPLATE
        .replace("<!--SUMMARY-->", &summary)
        .replace("<!--ROWS-->", &rows)
}

#[cfg(test)]
mod tests {
    use super::*;
    use slop_ninja_protocol::{ChainBinding, FINNEY_GENESIS, TESTNET_GENESIS};

    #[test]
    fn only_a_testnet_record_carries_the_testnet_promotion_label() {
        let chain = |genesis| {
            Chain::from_binding(&ChainBinding {
                genesis,
                netuid: 601,
                mechanism: 0,
            })
        };
        assert_eq!(
            PromotionRule::of(&chain(TESTNET_GENESIS)),
            Some(PromotionRule::testnet())
        );
        assert_eq!(PromotionRule::of(&chain(FINNEY_GENESIS)), None);
        let mut upper = chain(TESTNET_GENESIS);
        upper.genesis = format!("0x{}", upper.genesis.to_ascii_uppercase());
        assert!(PromotionRule::of(&upper).is_some());
    }
}
