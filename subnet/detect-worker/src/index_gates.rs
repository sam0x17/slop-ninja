//! The index gates (`index-gates/v2`): whether a public page may be indexed,
//! listed in a sitemap shard and announced through IndexNow.
//!
//! Which pages these gates judge follows from how the text arrived, never
//! from what it says: a check of a public page's URL (a web page, a Bluesky
//! post or a post on X) makes a public page, and only public pages are ever
//! indexed; pasted text makes a secret page, which never is, even when its
//! words are those of a public page.
//!
//! Every live public page is indexable, whatever its length or platform and
//! whatever its ranking or the display policy's narrowing. What keeps a
//! page out is a consent, legal or safety limit, never a visibility choice.
//! Four gates, computed at render ([`robots`]) and at shard build (the same
//! rules in SQL, [`crate::sitemap::select_shard`]). They never read the
//! text, the band, the confidence or the share:
//!
//! 1. the page is live: not pending, failed, withheld (`excerpt_withheld`),
//!    removed, disabled, deleted or a legacy `expired` tombstone, and not a
//!    post on X while `DETECT_X` is `purged` (such a page reads as deleted);
//! 2. its source has not opted out and does not ask not to be indexed: the
//!    source's state is `ok` or `gone`, not `opted_out` or `unlisted` (a
//!    page with no source row fails too). A fetched page that asks not to
//!    be indexed, on its host or after a redirect, never goes live
//!    (`private_source`), and capability URLs never reach a page (the URL
//!    rules refuse them before a slug is drawn);
//! 3. its host has no opt-out row;
//! 4. its registrable domain is not blocked by 1.1.1.1 for Families
//!    (malware and adult): reputation `ranked` or `unranked`, not `adult`.
//!
//! Overrides: `DETECT_INDEX` off (the operator's emergency switch) and
//! `index_force = off` make every page `noindex`. `index_force = on` lifts
//! gate 4 alone, the operator's recorded judgment that the family screen
//! misread a domain; it never indexes a page that is not live or whose
//! source or host opted out. Secret pages are never public pages: their
//! robots are `noindex_all` ([`crate::page`]), and no sitemap or IndexNow
//! ping names them.

use slop_ninja_detect_page::codes::{DetState, IndexForce, Reputation, Robots, SourceStatus};

/// The gates' version.
pub const INDEX_GATES_V: &str = "index-gates/v2";

/// The source states gate 2 passes. The SQL reads the same list
/// ([`sql_in`]), so a new state fails both until it is added here.
pub const SOURCE_STATES: &[SourceStatus] = &[SourceStatus::Ok, SourceStatus::Gone];

/// The reputations gate 4 passes (every one but the family screen's block).
pub const REPUTATIONS: &[Reputation] = &[Reputation::Ranked, Reputation::Unranked];

/// What the gates read of a page.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GateInput {
    /// The state the page renders in (a post on X under `DETECT_X =
    /// purged` already reads as deleted).
    pub state: DetState,
    /// The source's state and reputation (`None` with no source row).
    pub source: Option<(SourceStatus, Reputation)>,
    /// Whether the page's host has an opt-out row.
    pub host_opted_out: bool,
    pub index_force: Option<IndexForce>,
    /// `DETECT_INDEX`.
    pub index_on: bool,
}

/// Whether each of the four gates passes, in order.
pub fn gates(g: &GateInput) -> [bool; 4] {
    let g1 = g.state == DetState::Live;
    let g2 = g.source.is_some_and(|(s, _)| SOURCE_STATES.contains(&s));
    let g3 = !g.host_opted_out;
    let g4 = g.source.is_some_and(|(_, r)| REPUTATIONS.contains(&r));
    [g1, g2, g3, g4]
}

/// Whether the page is indexed, overrides included.
pub fn indexed(g: &GateInput) -> bool {
    if !g.index_on || g.index_force == Some(IndexForce::Off) {
        return false;
    }
    let [live, source, host, family] = gates(g);
    live && source && host && (family || g.index_force == Some(IndexForce::On))
}

/// The page's robots directive.
pub fn robots(g: &GateInput) -> Robots {
    if indexed(g) { Robots::Index } else { Robots::Noindex }
}

/// `'a','b'` for an SQL `IN (...)` over code values (each a fixed
/// lowercase code, so nothing needs quoting).
pub fn sql_in<T: Copy>(values: &[T], code: impl Fn(T) -> &'static str) -> String {
    values.iter().map(|v| format!("'{}'", code(*v))).collect::<Vec<_>>().join(",")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn passing() -> GateInput {
        GateInput {
            state: DetState::Live,
            source: Some((SourceStatus::Ok, Reputation::Ranked)),
            host_opted_out: false,
            index_force: None,
            index_on: true,
        }
    }

    #[test]
    fn every_kept_gate_alone_keeps_a_page_out() {
        assert!(indexed(&passing()));
        assert_eq!(gates(&passing()), [true; 4]);
        let mut fails: Vec<(String, GateInput)> = vec![
            ("unlisted".into(), GateInput { source: Some((SourceStatus::Unlisted, Reputation::Ranked)), ..passing() }),
            ("opted out".into(), GateInput { source: Some((SourceStatus::OptedOut, Reputation::Ranked)), ..passing() }),
            ("no source".into(), GateInput { source: None, ..passing() }),
            ("host opt-out".into(), GateInput { host_opted_out: true, ..passing() }),
            ("adult".into(), GateInput { source: Some((SourceStatus::Ok, Reputation::Adult)), ..passing() }),
            ("index off".into(), GateInput { index_on: false, ..passing() }),
            ("force off".into(), GateInput { index_force: Some(IndexForce::Off), ..passing() }),
        ];
        for state in DetState::ALL.iter().copied().filter(|s| *s != DetState::Live) {
            fails.push((state.as_str().into(), GateInput { state, ..passing() }));
        }
        for (why, g) in fails {
            assert!(!indexed(&g), "{why}");
            assert_eq!(robots(&g), Robots::Noindex, "{why}");
        }
    }

    #[test]
    fn the_dropped_gates_keep_no_page_out() {
        // Ranking and a gone source were gates 2 and 4 of v1; length,
        // platform and the display narrowing are not inputs at all.
        for g in [
            GateInput { source: Some((SourceStatus::Ok, Reputation::Unranked)), ..passing() },
            GateInput { source: Some((SourceStatus::Gone, Reputation::Ranked)), ..passing() },
            GateInput { source: Some((SourceStatus::Gone, Reputation::Unranked)), ..passing() },
        ] {
            assert!(indexed(&g), "{g:?}");
        }
    }

    #[test]
    fn force_on_lifts_the_family_screen_only() {
        let on = |g: GateInput| GateInput { index_force: Some(IndexForce::On), ..g };
        assert!(indexed(&on(GateInput { source: Some((SourceStatus::Ok, Reputation::Adult)), ..passing() })));
        assert!(indexed(&on(passing())));
        // Never a page that is not live, a source or host that opted out,
        // or with indexing off.
        let mut kept = vec![
            GateInput { source: Some((SourceStatus::OptedOut, Reputation::Ranked)), ..passing() },
            GateInput { source: Some((SourceStatus::Unlisted, Reputation::Adult)), ..passing() },
            GateInput { source: None, ..passing() },
            GateInput { host_opted_out: true, ..passing() },
            GateInput { index_on: false, ..passing() },
        ];
        for state in DetState::ALL.iter().copied().filter(|s| *s != DetState::Live) {
            kept.push(GateInput { state, ..passing() });
        }
        for g in kept {
            assert!(!indexed(&on(g)), "{g:?}");
        }
    }

    #[test]
    fn the_sql_lists_are_the_gates_lists() {
        assert_eq!(sql_in(SOURCE_STATES, SourceStatus::as_str), "'ok','gone'");
        assert_eq!(sql_in(REPUTATIONS, Reputation::as_str), "'ranked','unranked'");
        for s in SourceStatus::ALL {
            let g = GateInput { source: Some((*s, Reputation::Ranked)), ..passing() };
            assert_eq!(
                gates(&g)[1],
                sql_in(SOURCE_STATES, SourceStatus::as_str).contains(&format!("'{}'", s.as_str()))
            );
        }
        for r in Reputation::ALL {
            let g = GateInput { source: Some((SourceStatus::Ok, *r)), ..passing() };
            assert_eq!(gates(&g)[3], sql_in(REPUTATIONS, Reputation::as_str).contains(&format!("'{}'", r.as_str())));
        }
    }

    #[test]
    fn the_gates_never_read_the_band() {
        // GateInput holds no band, confidence, share, length or platform:
        // the type is the proof; this test pins its fields so a new one is
        // reviewed here.
        let GateInput { state: _, source: _, host_opted_out: _, index_force: _, index_on: _ } = passing();
    }
}
