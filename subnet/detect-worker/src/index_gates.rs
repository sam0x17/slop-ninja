//! The index gates (`index-gates/v1`): whether a public page may be indexed
//! and listed in a sitemap shard.
//!
//! Five structural gates, computed at render ([`robots`]) and at shard build
//! (the same rules in SQL, [`crate::sitemap::SELECT_SHARD`]). They never
//! read the band, the confidence or the share:
//!
//! 1. the fetch succeeded, the text reached the floor, and it is long-form:
//!    a web page whose scored part has at least [`LONG_FORM_WORDS`] words
//!    (posts on Bluesky and X, and short results, stay `noindex`);
//! 2. the source is indexable: its state is `ok` (not `unlisted`,
//!    `opted_out` or `gone`). Capability URLs never reach a page (the URL
//!    rules refuse them before a slug is drawn), and a source that asks not
//!    to be indexed fails its check (`private_source`), so neither can pass;
//! 3. no opt-out and no suppression: the page is `live` (not
//!    `excerpt_withheld`, `removed` or any other state), and its host has no
//!    opt-out row;
//! 4. the registrable domain is in the ranking and not blocked by 1.1.1.1
//!    for Families: reputation `ranked`;
//! 5. the page carries the full analysis: its passages are shown (the
//!    display policy did not narrow it, and `DETECT_EXCERPTS` is on).
//!
//! Overrides: `DETECT_INDEX` off and `index_force = off` make every page
//! `noindex`; `index_force = on` indexes a live web page whatever gates 2
//! to 5 say. A page that is not live, and every post on Bluesky or X, is
//! never indexed.

use slop_ninja_detect_page::codes::{DetState, IndexForce, Platform, Reputation, Robots, SourceStatus};

/// The gates' version.
pub const INDEX_GATES_V: &str = "index-gates/v1";

/// The fewest scored words of a long-form page: the measured band table's
/// floor. Below it the short-text rule applies and the page shows no
/// figures, so it is not long-form.
pub const LONG_FORM_WORDS: u32 = 150;

/// What the gates read of a page.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GateInput {
    pub state: DetState,
    pub platform: Platform,
    /// The scored part's words (`None` with no run).
    pub words: Option<u32>,
    /// Whether the run is a short result (the short-text rule applied).
    pub short: bool,
    /// The source's state and reputation (`None` with no source row).
    pub source: Option<(SourceStatus, Reputation)>,
    /// Whether the page's host has an opt-out row.
    pub host_opted_out: bool,
    /// Gate 5: the page shows its passages.
    pub full_analysis: bool,
    pub index_force: Option<IndexForce>,
    /// `DETECT_INDEX`.
    pub index_on: bool,
}

/// Whether each of the five gates passes, in order.
pub fn gates(g: &GateInput) -> [bool; 5] {
    let g1 = g.platform == Platform::Web && !g.short && g.words.is_some_and(|w| w >= LONG_FORM_WORDS);
    let g2 = g.source.is_some_and(|(s, _)| s == SourceStatus::Ok);
    let g3 = g.state == DetState::Live && !g.host_opted_out;
    let g4 = g.source.is_some_and(|(_, r)| r == Reputation::Ranked);
    let g5 = g.full_analysis;
    [g1, g2, g3, g4, g5]
}

/// Whether the page is indexed, overrides included.
pub fn indexed(g: &GateInput) -> bool {
    if !g.index_on || g.index_force == Some(IndexForce::Off) || g.state != DetState::Live {
        return false;
    }
    if g.platform != Platform::Web {
        return false;
    }
    if g.index_force == Some(IndexForce::On) {
        return true;
    }
    gates(g).iter().all(|x| *x)
}

/// The page's robots directive.
pub fn robots(g: &GateInput) -> Robots {
    if indexed(g) { Robots::Index } else { Robots::Noindex }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn passing() -> GateInput {
        GateInput {
            state: DetState::Live,
            platform: Platform::Web,
            words: Some(400),
            short: false,
            source: Some((SourceStatus::Ok, Reputation::Ranked)),
            host_opted_out: false,
            full_analysis: true,
            index_force: None,
            index_on: true,
        }
    }

    #[test]
    fn every_gate_alone_keeps_a_page_out() {
        assert!(indexed(&passing()));
        assert_eq!(gates(&passing()), [true; 5]);
        let fails: Vec<(&str, GateInput)> = vec![
            ("bluesky", GateInput { platform: Platform::Bluesky, ..passing() }),
            ("x", GateInput { platform: Platform::X, ..passing() }),
            ("short result", GateInput { short: true, words: Some(100), ..passing() }),
            ("149 words", GateInput { words: Some(LONG_FORM_WORDS - 1), ..passing() }),
            ("no run", GateInput { words: None, ..passing() }),
            ("unlisted", GateInput { source: Some((SourceStatus::Unlisted, Reputation::Ranked)), ..passing() }),
            ("opted out", GateInput { source: Some((SourceStatus::OptedOut, Reputation::Ranked)), ..passing() }),
            ("gone", GateInput { source: Some((SourceStatus::Gone, Reputation::Ranked)), ..passing() }),
            ("no source", GateInput { source: None, ..passing() }),
            ("host opt-out", GateInput { host_opted_out: true, ..passing() }),
            ("withheld", GateInput { state: DetState::ExcerptWithheld, ..passing() }),
            ("pending", GateInput { state: DetState::Pending, ..passing() }),
            ("unranked", GateInput { source: Some((SourceStatus::Ok, Reputation::Unranked)), ..passing() }),
            ("adult", GateInput { source: Some((SourceStatus::Ok, Reputation::Adult)), ..passing() }),
            ("positions only", GateInput { full_analysis: false, ..passing() }),
            ("index off", GateInput { index_on: false, ..passing() }),
            ("force off", GateInput { index_force: Some(IndexForce::Off), ..passing() }),
        ];
        for (why, g) in fails {
            assert!(!indexed(&g), "{why}");
            assert_eq!(robots(&g), Robots::Noindex, "{why}");
        }
    }

    #[test]
    fn force_on_overrides_the_source_gates_only() {
        let on = |g: GateInput| GateInput { index_force: Some(IndexForce::On), ..g };
        for g in [
            GateInput { source: Some((SourceStatus::Ok, Reputation::Unranked)), ..passing() },
            GateInput { full_analysis: false, ..passing() },
            GateInput { short: true, words: Some(60), ..passing() },
        ] {
            assert!(indexed(&on(g)));
        }
        // Never a post, a page that is not live, or with indexing off.
        for g in [
            GateInput { platform: Platform::X, ..passing() },
            GateInput { platform: Platform::Bluesky, ..passing() },
            GateInput { state: DetState::Removed, ..passing() },
            GateInput { index_on: false, ..passing() },
        ] {
            assert!(!indexed(&on(g)));
        }
    }

    #[test]
    fn the_gates_never_read_the_band() {
        // GateInput holds no band, confidence or share: the type is the proof;
        // this test pins its fields so a new one is reviewed here.
        let GateInput {
            state: _,
            platform: _,
            words: _,
            short: _,
            source: _,
            host_opted_out: _,
            full_analysis: _,
            index_force: _,
            index_on: _,
        } = passing();
    }
}
