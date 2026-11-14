//! The display policy (`display-v1`): what of a stored run a page shows.
//!
//! It applies at render time to every page, unlike the run, which is fixed
//! when it is scored: a legal or policy change reaches old pages at their
//! next render. A public run's document holds at most what the widest
//! policy shows (the passages and their context, or for bands 1 and 2 the
//! first sentence), so a policy change can only narrow a page, never widen
//! it.
//!
//! Public pages, in order (the first match decides):
//! - a withheld excerpt (`excerpt_withheld`): no body and no positions;
//! - a source that asks not to be listed (`unlisted`): no body and no
//!   positions, and the source shows its domain only;
//! - a source outside the ranking (`unranked`, `adult`): no body and no
//!   title, the address as plain text (no link);
//! - a source that is gone: no body, every passage listed by position
//!   (unlinked by the renderer);
//! - `DETECT_EXCERPTS` off: as a gone source, positions only;
//! - otherwise the document's body, anchors and listed passages.
//!
//! Secret pages and the unstored result show the whole text the submitter
//! pasted, plain.
//!
//! Robots: a public page is indexed only when it is live, not an X or
//! Bluesky post, its source is ranked and listed and not gone, and
//! `DETECT_INDEX` is on; the operator's `index_force` overrides the source
//! rules (`on`) or every rule (`off`).

use slop_ninja_detect_page::codes::{
    Adapter, AnchorRole, DetState, IndexForce, Platform, Reputation, Robots, Shown, SourceStatus, SourceVia,
    WithheldReason,
};
use slop_ninja_detect_page::page::{FurtherPassage, Source, Text};
use slop_ninja_detect_page::x::X_HOST;

use crate::db::Row;
use crate::doc::{RunDoc, SourceCheck};

/// The policy's version name (`runs.display_v`).
pub const DISPLAY_V: &str = "display_v1";

/// The switches a page read depends on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PageSwitches {
    /// `DETECT_PAGES`: off answers every stored page `pages_off`.
    pub pages: bool,
    /// `DETECT_CARDS = generic`: no ninja state on pages and cards.
    pub cards_generic: bool,
    /// `DETECT_INDEX`: off makes every page `noindex`.
    pub index: bool,
    /// `DETECT_EXCERPTS`: off shows no passage text on public pages.
    pub excerpts: bool,
    /// `DETECT_X = purged`: every X page answers 410 `x_purged`.
    pub x_purged: bool,
}

impl PageSwitches {
    /// Every switch at its default.
    pub const DEFAULT: PageSwitches =
        PageSwitches { pages: true, cards_generic: false, index: true, excerpts: true, x_purged: false };
}

/// A `sources` row as a page reads it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceRow {
    pub source_id: u64,
    pub canon_url: Option<String>,
    pub host: String,
    pub adapter: Adapter,
    pub title: Option<String>,
    pub site_name: Option<String>,
    pub author: Option<String>,
    pub published_at: Option<u64>,
    pub fetched_at: Option<u64>,
    pub fresh_until: Option<u64>,
    pub state: SourceStatus,
    pub reputation: Reputation,
}

/// The columns [`SourceRow::from_row`] reads, from `sources s`.
pub const SOURCE_COLUMNS: &str = "s.source_id AS source_id, s.canon_url AS canon_url, s.host AS host, \
s.adapter AS adapter, s.title AS title, s.site_name AS site_name, s.author AS author, \
s.published_at AS published_at, s.fetched_at AS fetched_at, s.fresh_until AS fresh_until, s.state AS sstate, \
s.reputation AS reputation";

impl SourceRow {
    pub fn from_row(r: &Row) -> Result<SourceRow, crate::store::BadRow> {
        Ok(SourceRow {
            source_id: r.u64("source_id").ok_or(crate::store::BadRow)?,
            canon_url: r.text("canon_url").map(str::to_owned),
            host: r.text("host").ok_or(crate::store::BadRow)?.to_owned(),
            adapter: Adapter::parse(r.text("adapter").ok_or(crate::store::BadRow)?).ok_or(crate::store::BadRow)?,
            title: r.text("title").map(str::to_owned),
            site_name: r.text("site_name").map(str::to_owned),
            author: r.text("author").map(str::to_owned),
            published_at: r.u64("published_at"),
            fetched_at: r.u64("fetched_at"),
            fresh_until: r.u64("fresh_until"),
            state: SourceStatus::parse(r.text("sstate").ok_or(crate::store::BadRow)?).ok_or(crate::store::BadRow)?,
            reputation: Reputation::parse(r.text("reputation").ok_or(crate::store::BadRow)?)
                .ok_or(crate::store::BadRow)?,
        })
    }

    /// The adapter the source was read through, as the page names it.
    pub fn via(&self) -> SourceVia {
        match self.adapter {
            Adapter::Web => SourceVia::Web,
            Adapter::Bluesky => SourceVia::Bluesky,
            Adapter::X => SourceVia::X,
        }
    }
}

/// A pasted text's source line: the host only, not checked.
pub fn submitter_source(sc: &SourceCheck) -> Source {
    Source {
        host: sc.host.clone(),
        url: None,
        title: None,
        site_name: None,
        author: None,
        published_at: None,
        fetched_at: None,
        via: SourceVia::Submitter,
        status: None,
        reputation: None,
        link: false,
        checked: sc.checked,
        page_hash: None,
        stale: false,
    }
}

/// The domain only (pending and failed public pages, unlisted sources).
pub fn domain_only(host: &str, via: SourceVia, status: Option<SourceStatus>) -> Source {
    Source {
        host: host.to_owned(),
        url: None,
        title: None,
        site_name: None,
        author: None,
        published_at: None,
        fetched_at: None,
        via,
        status,
        reputation: None,
        link: false,
        checked: false,
        page_hash: None,
        stale: false,
    }
}

/// A secret page's text block: the whole text, plain.
pub fn secret_text(text: &str) -> Text {
    Text {
        body: Some(text.to_owned()),
        shown: Shown::Full,
        anchors: Vec::new(),
        further: Vec::new(),
        page_words: None,
        withheld_reason: None,
    }
}

fn withheld(reason: WithheldReason, further: Vec<FurtherPassage>, page_words: u32) -> Text {
    Text {
        body: None,
        shown: Shown::Withheld,
        anchors: Vec::new(),
        further,
        page_words: Some(page_words),
        withheld_reason: Some(reason),
    }
}

/// Every passage of `doc` by position: the quoted ones (with their run
/// confidence) and the listed ones, in word order.
fn positions(doc: &RunDoc) -> Vec<FurtherPassage> {
    let mut out: Vec<FurtherPassage> = doc
        .anchors
        .iter()
        .filter(|a| a.role == AnchorRole::Passage)
        .filter_map(|a| {
            let fragment = a.fragment.clone()?;
            let confidence =
                doc.result_full.spans.iter().find(|s| s.start == a.start && s.end == a.end).and_then(|s| s.confidence);
            Some(FurtherPassage { from_word: a.from_word, to_word: a.to_word, fragment, confidence })
        })
        .collect();
    out.extend(doc.further.iter().cloned());
    out.sort_by_key(|f| f.from_word);
    out
}

/// What a public page shows: its text block, and whether its passages stay
/// drawn (`false` clears the run's spans, which point into the body).
pub fn public_text(state: DetState, src: &SourceRow, doc: &RunDoc, sw: PageSwitches) -> (Text, bool) {
    let words = doc.page_words;
    if state == DetState::ExcerptWithheld {
        return (withheld(WithheldReason::LegalNotice, Vec::new(), words), false);
    }
    if src.state == SourceStatus::Unlisted || src.state == SourceStatus::OptedOut {
        return (withheld(WithheldReason::Unlisted, Vec::new(), words), false);
    }
    if src.reputation != Reputation::Ranked {
        return (withheld(WithheldReason::Unranked, Vec::new(), words), false);
    }
    if src.state == SourceStatus::Gone {
        return (withheld(WithheldReason::SourceGone, positions(doc), words), false);
    }
    if !sw.excerpts {
        // No reason code names the switch yet; the page shows positions as
        // for a withheld excerpt.
        return (withheld(WithheldReason::LegalNotice, positions(doc), words), false);
    }
    let text = Text {
        body: Some(doc.body.clone()),
        shown: Shown::Excerpt,
        anchors: doc.anchors.clone(),
        further: doc.further.clone(),
        page_words: (src.adapter == Adapter::Web).then_some(words),
        withheld_reason: None,
    };
    (text, true)
}

/// A public page's source block, under the same rules.
pub fn public_source(state: DetState, src: &SourceRow, page_hash: Option<&str>, now: u64) -> Source {
    let via = src.via();
    if src.adapter == Adapter::X {
        // An X page's only X URL is its post link: the source is the host.
        let mut s = domain_only(X_HOST, SourceVia::X, None);
        if state == DetState::Live {
            s.status = Some(src.state);
            s.reputation = Some(src.reputation);
            s.fetched_at = src.fetched_at;
            s.checked = true;
        }
        return s;
    }
    if !matches!(state, DetState::Live | DetState::ExcerptWithheld) {
        return domain_only(&src.host, via, None);
    }
    if src.state == SourceStatus::Unlisted || src.state == SourceStatus::OptedOut {
        return domain_only(&src.host, via, Some(src.state));
    }
    let ranked = src.reputation == Reputation::Ranked;
    Source {
        host: src.host.clone(),
        url: src.canon_url.clone(),
        title: if ranked { src.title.clone() } else { None },
        site_name: src.site_name.clone(),
        author: src.author.clone(),
        published_at: src.published_at,
        fetched_at: src.fetched_at,
        via,
        status: Some(src.state),
        reputation: Some(src.reputation),
        link: ranked,
        checked: true,
        page_hash: page_hash.map(str::to_owned),
        stale: src.fresh_until.is_some_and(|t| t <= now),
    }
}

/// A public page's robots directive.
pub fn public_robots(
    state: DetState,
    platform: Platform,
    src: Option<&SourceRow>,
    index_force: Option<IndexForce>,
    sw: PageSwitches,
) -> Robots {
    if index_force == Some(IndexForce::Off) || !sw.index || state != DetState::Live {
        return Robots::Noindex;
    }
    if matches!(platform, Platform::X | Platform::Bluesky) {
        return Robots::Noindex;
    }
    if index_force == Some(IndexForce::On) {
        return Robots::Index;
    }
    match src {
        Some(s) if s.state == SourceStatus::Ok && s.reputation == Reputation::Ranked => Robots::Index,
        _ => Robots::Noindex,
    }
}
