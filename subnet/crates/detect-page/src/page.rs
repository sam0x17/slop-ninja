//! The page contract `slop_ninja_detect_page_v2`: one JSON shape for every
//! page the website and the secret host render.
//!
//! The detect Worker produces it with the display policy already applied,
//! for stored pages (`/internal/v1/page`), the unstored result
//! (`/internal/v1/preview`) and the example reports (`/internal/v1/example`).
//! Nothing per-user is ever in it, so a rendered page can be cached for
//! every reader. Every timestamp is integer milliseconds UTC. Every text
//! offset is a UTF-16 offset into `text.body`. Every field is always present
//! (`null` when it does not apply), so a reader sees one shape.
//!
//! Hosts and URLs carry no origin: `host` is a role and `canonical`,
//! `superseded_by` and `supersedes` are public slugs. The renderer builds
//! absolute URLs from its configured origins ([`crate::Origins`]).
//!
//! A post on X checked from its link ([`Page::is_x_post`]) is a public page
//! of `platform = x` that keeps no post text: `text` is `null`, the run has
//! no passages, and the post is shown from [`Page::embed`], X's own markup
//! made typed ([`crate::x`]). It is always `noindex`, never in a sitemap,
//! and its page kind carries no ad slot.
//!
//! [`Page::check`] states the invariants a producer keeps and a consumer may
//! rely on.

use serde::{Deserialize, Serialize};

use crate::codes::*;
use crate::ninja::{NinjaInput, ninja_state};
use crate::refusal::RefusalCode;
use crate::slug;
pub use crate::x::XEmbed;
use crate::x::{X_HOST, parse_x_canonical};

/// The schema id.
pub const PAGE_SCHEMA: &str = "slop_ninja_detect_page_v2";

/// The literal [`PAGE_SCHEMA`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PageSchema;

impl Serialize for PageSchema {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(PAGE_SCHEMA)
    }
}

impl<'de> Deserialize<'de> for PageSchema {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        if s == PAGE_SCHEMA { Ok(PageSchema) } else { Err(serde::de::Error::custom(format!("schema {s:?}"))) }
    }
}

/// One page.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Page {
    pub schema: PageSchema,
    /// The page's slug: 7 characters on a public page, 20 on a secret one;
    /// `null` for the unstored result and the example reports.
    pub slug: Option<String>,
    /// The host the page belongs to.
    pub host: HostRole,
    pub kind: Kind,
    pub platform: Platform,
    pub state: DetState,
    /// Bumped by every state change; part of the ETag and the card URL.
    pub state_v: u32,
    /// `null` for the unstored result.
    pub created_at: Option<u64>,
    /// `null`: until deleted.
    pub expires_at: Option<u64>,
    pub robots: Robots,
    /// The public slug whose URL is canonical (`rel=canonical`); `null` on
    /// secret, unstored and example pages.
    pub canonical: Option<String>,
    /// The public slug of the newest run of this source, when newer.
    pub superseded_by: Option<String>,
    /// The public slug this page supersedes.
    pub supersedes: Option<String>,
    /// The example slot (1 to 4) of an example report.
    pub example_slot: Option<u8>,
    /// The package serving now.
    pub serving: Serving,
    /// The run the page shows; `null` while pending, after a failure, and on
    /// every ended page (removed, disabled, deleted, expired).
    pub run: Option<Run>,
    /// A paid whole document's sections.
    pub sections: Option<Sections>,
    /// The text block; always `null` on an X page, which keeps no post text.
    pub text: Option<Text>,
    /// An X page's static quote of the post, built from X's embed markup on
    /// a page-cache miss and never stored. `null` on every other page, on an
    /// adult post (no static quote) and when X could not be reached (see
    /// [`Page::x_view`]).
    pub embed: Option<XEmbed>,
    /// A public page's source; a pasted text's `{host, via: submitter,
    /// checked: false}`, whatever the host; `null` on other secret pages.
    pub source: Option<Source>,
    /// What a pending page shows.
    pub pending: Option<Pending>,
    /// Why a failed page failed.
    pub failure: Option<Failure>,
    /// Why an ended page ended, when its copy is not its state's own line:
    /// `x_gone` on an X page deleted because the post went (410, "This post
    /// is no longer available on X, so its report was removed."). `null`
    /// otherwise.
    pub ended: Option<Ended>,
    /// The removal behind a `removed` or `excerpt_withheld` page.
    pub removal: Option<Removal>,
    /// The page's card, when it has one.
    pub card: Option<CardRef>,
    /// The ninja's state; `null` for paid documents and when cards are
    /// generic.
    pub ninja: Option<NinjaState>,
    /// The feedback receipt, on live pages and the unstored result.
    pub receipt: Option<String>,
}

/// The package serving now.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Serving {
    /// "span package d24".
    pub package_label: String,
    /// The published digest's first 8 hex digits.
    pub package_pub_short: String,
    pub publication_pending: bool,
    /// The page's run used another package: show the re-run banner.
    pub differs_from_run: bool,
}

/// The package a run used.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackageLine {
    pub label: String,
    /// The published package digest, 64 hex digits.
    pub published_digest: String,
    pub publication_pending: bool,
}

/// One scored run, with the values the page prints fixed at scoring time.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Run {
    pub scored_at: u64,
    pub package: PackageLine,
    /// The scorer build's short digest, for the record.
    pub scorer_build: String,
    /// The result policy, e.g. `long-v1`.
    pub policy: String,
    /// The evidence table the rates come from.
    pub evidence: String,
    /// The band, for a text of 150 words and more; `null` for a short text
    /// and a paid document.
    pub band: Option<Band>,
    /// A short text's result.
    pub short: Option<ShortResult>,
    /// Confidence floored to tenths of a percent (`null` when short or paid).
    pub confidence_tenths: Option<u16>,
    /// "97.3%", "above 99.9%", "below 1%".
    pub confidence_text: Option<String>,
    /// The dial's bare number: the share floored, never above the
    /// confidence.
    pub share_pct: Option<u8>,
    /// "about 86%" or "below 1%".
    pub share_text: Option<String>,
    /// Whitespace words scored (a paid document: all of them).
    pub words: u32,
    /// Whitespace words of the whole text.
    pub total_words: u32,
    pub cut: CutKind,
    pub tokens: u32,
    /// `null` for a paid document.
    pub length: Option<LengthBin>,
    /// Highlighted passages (UTF-16 offsets into `text.body`), in order.
    #[serde(default)]
    pub spans: Vec<Span>,
    /// Why no passage is drawn.
    pub spans_note: Option<SpansNote>,
    /// Control markers named in the text.
    #[serde(default)]
    pub markers: Vec<String>,
    /// The text holds invisible or control characters.
    pub invisible: bool,
}

/// One highlighted passage.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Span {
    pub start: u32,
    pub end: u32,
    pub words: u32,
    /// Whole percent, floored; `null` on a short text (one neutral shade).
    pub confidence: Option<u8>,
}

/// A paid whole document's sections.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Sections {
    pub items: Vec<Section>,
    pub document: DocumentFigures,
}

/// One section, by the words it owns.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Section {
    /// From 1.
    pub index: u32,
    /// First and last owned word, from 1.
    pub owned_from_word: u32,
    pub owned_to_word: u32,
    /// The owned range in `text.body` (UTF-16), for the dividers.
    pub start: u32,
    pub end: u32,
    pub band: Band,
    pub confidence_tenths: u16,
    pub confidence_text: String,
    pub share_pct: u8,
    pub share_text: String,
    pub spans_count: u32,
}

/// A paid document's figures.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocumentFigures {
    /// The owned-word-weighted share, for the neutral dial.
    pub share_pct: u8,
    pub share_text: String,
    /// Sections in band 3 or 4.
    pub strong_sections: u32,
    pub total_sections: u32,
}

/// The text a page shows.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Text {
    /// The whole text (secret pages), the quoted passages and their context
    /// (public pages), or `null` when withheld.
    pub body: Option<String>,
    pub shown: Shown,
    /// What each range of a public page's quoted body is.
    #[serde(default)]
    pub anchors: Vec<Anchor>,
    /// Passages listed by word range and linked, not quoted.
    #[serde(default)]
    pub further: Vec<FurtherPassage>,
    /// The source page's words (public pages).
    pub page_words: Option<u32>,
    pub withheld_reason: Option<WithheldReason>,
}

/// A range of a public page's quoted body.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Anchor {
    /// UTF-16 offsets into `text.body`.
    pub start: u32,
    pub end: u32,
    pub role: AnchorRole,
    /// The words of the source page the range quotes, from 1.
    pub from_word: u32,
    pub to_word: u32,
    /// The text-fragment directive for "continue at the source"
    /// (`#:~:text=` value, percent-encoded).
    pub fragment: Option<String>,
}

/// A passage listed, not quoted.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FurtherPassage {
    pub from_word: u32,
    pub to_word: u32,
    pub fragment: String,
    pub confidence: Option<u8>,
}

/// Where a public text came from.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Source {
    /// The host in A-label form.
    pub host: String,
    pub url: Option<String>,
    pub title: Option<String>,
    pub site_name: Option<String>,
    pub author: Option<String>,
    pub published_at: Option<u64>,
    pub fetched_at: Option<u64>,
    pub via: SourceVia,
    pub status: Option<SourceStatus>,
    pub reputation: Option<Reputation>,
    /// Whether the page may link the source ("Read the original").
    pub link: bool,
    /// A submitter's source is shown "Not checked." until a fetch confirms it.
    pub checked: bool,
    /// The fetched page's hash, for the record.
    pub page_hash: Option<String>,
    /// Past its freshness: offer "Check the page again".
    pub stale: bool,
}

/// What a pending page shows.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pending {
    /// The claim's lease ended: offer "Run it".
    pub lease_ended: bool,
    /// A paid job's progress.
    pub job: Option<JobProgress>,
}

/// A paid job's progress.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobProgress {
    pub sections_done: u32,
    pub sections_total: u32,
}

/// Why a page failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Failure {
    pub code: RefusalCode,
}

/// Why an ended page ended; the renderer prints the code's copy in place of
/// the state's own line, with the state's status.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ended {
    /// One of [`ENDED_CODES`].
    pub code: RefusalCode,
}

/// The codes an [`Ended`] may carry.
pub const ENDED_CODES: &[RefusalCode] = &[RefusalCode::XGone];

/// How an X page shows its post ([`Page::x_view`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum XView<'a> {
    /// The static quote, in its frame with "View on X" and the click-to-load
    /// link.
    Quote(&'a XEmbed),
    /// An adult post: no static quote, only "View on X" and the click-to-load
    /// link.
    Adult,
    /// A withheld excerpt: no quote, only "View on X".
    Withheld,
    /// X could not be reached: "The post couldn't be loaded from X right now.
    /// View it on X ↗", served `no-store` and kept out of the page cache, so
    /// the next view tries again.
    Unavailable,
}

/// A removal.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Removal {
    pub scope: RemovalScope,
    /// The basis by type, e.g. `copyright`.
    pub basis: String,
    /// The jurisdiction, e.g. `US`.
    pub jurisdiction: String,
    pub at: u64,
}

/// A page's card.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CardRef {
    pub variant: CardVariant,
    /// The card's path on the page's host, with `?v=<card_v>.<state_v>`.
    pub image: String,
}

/// A broken invariant of [`Page::check`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PageError(pub &'static str);

impl std::fmt::Display for PageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}

impl std::error::Error for PageError {}

fn need(ok: bool, what: &'static str) -> Result<(), PageError> {
    if ok { Ok(()) } else { Err(PageError(what)) }
}

/// The length of `s` in UTF-16 units.
pub fn utf16_len(s: &str) -> u32 {
    s.encode_utf16().count() as u32
}

impl Page {
    /// The HTTP status of the page: 451 for a
    /// removed or disabled page, 410 for a deleted or expired one, else 200.
    pub fn http_status(&self) -> u16 {
        match self.state {
            DetState::Removed | DetState::Disabled => 451,
            DetState::Deleted | DetState::Expired => 410,
            _ => 200,
        }
    }

    /// Whether the run is a paid whole document.
    pub fn is_paid_document(&self) -> bool {
        self.kind == Kind::PrivatePaid
    }

    /// Whether the page is a post on X checked from its link (`public_url`,
    /// `platform = x`): no text block, no passages, always `noindex`, and the
    /// page kind `x_post`, which never carries an ad slot.
    pub fn is_x_post(&self) -> bool {
        self.kind == Kind::PublicUrl && self.platform == Platform::X
    }

    /// How a shown X page (live or with its excerpt withheld) shows the post;
    /// `None` for every other page.
    pub fn x_view(&self) -> Option<XView<'_>> {
        if !self.is_x_post() || !matches!(self.state, DetState::Live | DetState::ExcerptWithheld) {
            return None;
        }
        Some(match &self.embed {
            Some(e) => XView::Quote(e),
            None if self.state == DetState::ExcerptWithheld => XView::Withheld,
            None if self.source.as_ref().is_some_and(|s| s.reputation == Some(Reputation::Adult)) => XView::Adult,
            None => XView::Unavailable,
        })
    }

    /// Whether the page must be served `Cache-Control: no-store` and kept
    /// out of the page cache: an X page whose post could not be loaded.
    pub fn no_store(&self) -> bool {
        self.x_view() == Some(XView::Unavailable)
    }

    /// The ninja input of the run, if there is one.
    pub fn ninja_input(&self) -> Option<NinjaInput> {
        let run = self.run.as_ref()?;
        if self.is_paid_document() {
            return Some(NinjaInput::PaidDocument);
        }
        match (run.band, run.short, run.share_pct) {
            (Some(band), None, Some(share_pct)) => Some(NinjaInput::Banded { band, share_pct }),
            (None, Some(s), None) => Some(NinjaInput::Short(s)),
            _ => None,
        }
    }

    /// Checks the contract's invariants.
    pub fn check(&self) -> Result<(), PageError> {
        // Identity: slug, host, robots and canonical by kind.
        match self.kind {
            Kind::PublicUrl => {
                let s = self.slug.as_deref().ok_or(PageError("public page without slug"))?;
                need(slug::is_canonical(s, slug::PUBLIC_LEN), "public slug form")?;
                need(self.host == HostRole::Site, "public page on the site host")?;
                need(self.robots != Robots::NoindexAll, "public robots")?;
            }
            Kind::PrivateText | Kind::PrivatePaid => {
                let s = self.slug.as_deref().ok_or(PageError("secret page without slug"))?;
                need(slug::is_canonical(s, slug::SECRET_LEN), "secret slug form")?;
                need(self.host == HostRole::Secret, "secret page on the secret host")?;
                need(self.robots == Robots::NoindexAll, "secret robots")?;
                need(self.canonical.is_none() && self.superseded_by.is_none(), "secret page names no public page")?;
            }
            Kind::Unstored | Kind::Example => {
                need(self.slug.is_none(), "unstored and example pages have no slug")?;
                need(self.host == HostRole::Site, "unstored and example pages on the site host")?;
                need(self.robots == Robots::Noindex, "unstored and example robots")?;
                need(self.canonical.is_none(), "no canonical")?;
                need(self.card.is_none(), "no card")?;
            }
        }
        need((self.kind == Kind::Example) == self.example_slot.is_some(), "example slot")?;
        if let Some(n) = self.example_slot {
            need((1..=4).contains(&n), "example slot 1 to 4")?;
        }
        need(self.kind != Kind::Unstored || self.created_at.is_none(), "unstored has no created_at")?;
        need(matches!(self.kind, Kind::Unstored) || self.created_at.is_some(), "stored pages have created_at")?;
        for s in [&self.canonical, &self.superseded_by, &self.supersedes].into_iter().flatten() {
            need(slug::is_canonical(s, slug::PUBLIC_LEN), "canonical and supersession name public slugs")?;
        }
        need(self.robots != Robots::Index || self.state == DetState::Live, "only live pages are indexed")?;
        // State: what each state carries.
        let ended = self.state.is_ended();
        if ended || self.state == DetState::Pending || self.state == DetState::Failed {
            need(self.run.is_none() && self.sections.is_none(), "no run")?;
            need(self.card.is_none() && self.ninja.is_none() && self.receipt.is_none(), "no card, ninja or receipt")?;
            need(self.text.is_none(), "no text")?;
        }
        need((self.state == DetState::Pending) == self.pending.is_some(), "pending iff pending state")?;
        need((self.state == DetState::Failed) == self.failure.is_some(), "failure iff failed state")?;
        if let Some(e) = self.ended {
            need(ended && ENDED_CODES.contains(&e.code), "an ended reason only on an ended page")?;
            if e.code == RefusalCode::XGone {
                need(self.state == DetState::Deleted && self.is_x_post(), "x_gone ends a deleted X page")?;
            }
        }
        if matches!(self.state, DetState::Removed) && self.kind == Kind::PublicUrl {
            need(
                self.removal.as_ref().is_some_and(|r| r.scope == RemovalScope::Page),
                "a removed public page names its removal",
            )?;
        }
        if self.state == DetState::ExcerptWithheld {
            need(self.removal.as_ref().is_some_and(|r| r.scope == RemovalScope::Excerpt), "excerpt removal")?;
            need(self.robots == Robots::Noindex, "a withheld excerpt is noindex")?;
        }
        if ended {
            need(
                self.source.as_ref().is_none_or(|s| s.url.is_none() && s.title.is_none()),
                "an ended page shows no source",
            )?;
        }
        if let Some(p) = &self.pending {
            need(p.job.is_some() == self.is_paid_document(), "a pending paid page carries its job")?;
            if let Some(j) = p.job {
                need(j.sections_done <= j.sections_total, "job progress")?;
            }
        }
        if matches!(self.state, DetState::Pending | DetState::Failed) && self.kind == Kind::PublicUrl {
            let s = self.source.as_ref().ok_or(PageError("pending and failed public pages show the domain"))?;
            need(s.url.is_none() && s.title.is_none(), "the domain only")?;
        }
        self.check_x()?;
        if !matches!(self.state, DetState::Live | DetState::ExcerptWithheld) {
            return Ok(());
        }
        // A shown result.
        let run = self.run.as_ref().ok_or(PageError("a live page has a run"))?;
        need(self.text.is_some() || self.is_x_post(), "a live page has its text block")?;
        if self.is_paid_document() {
            let s = self.sections.as_ref().ok_or(PageError("a paid document has sections"))?;
            need(run.band.is_none() && run.short.is_none() && run.length.is_none(), "a paid document has no band")?;
            need(s.items.len() as u32 == s.document.total_sections && !s.items.is_empty(), "section count")?;
            let strong = s.items.iter().filter(|x| x.band.is_strong()).count() as u32;
            need(strong == s.document.strong_sections, "strong section count")?;
            for (i, x) in s.items.iter().enumerate() {
                need(
                    x.index == i as u32 + 1 && x.owned_from_word <= x.owned_to_word && x.start <= x.end,
                    "section order",
                )?;
            }
            need(self.ninja.is_none(), "a paid document has no ninja")?;
        } else {
            need(self.sections.is_none(), "sections only on paid documents")?;
            match (run.band, run.short) {
                (Some(_), None) => {
                    need(run.confidence_text.is_some() && run.share_text.is_some(), "banded figures")?;
                    need(run.confidence_tenths.is_some() && run.share_pct.is_some(), "banded numbers")?;
                    need(run.length.is_some_and(|l| !l.is_short()), "banded length")?;
                }
                (None, Some(_)) => {
                    need(
                        run.confidence_text.is_none() && run.share_text.is_none(),
                        "a short text prints no percentage",
                    )?;
                    need(run.confidence_tenths.is_none() && run.share_pct.is_none(), "a short text has no numbers")?;
                    need(run.length.is_some_and(|l| l.is_short()), "short length")?;
                    need(run.spans.iter().all(|s| s.confidence.is_none()), "short passages carry no confidence")?;
                }
                _ => return Err(PageError("exactly one of band and short")),
            }
            let strong = run.band.is_some_and(|b| b.is_strong()) || run.short == Some(ShortResult::Strong);
            need(strong || run.spans.is_empty(), "passages only in the strong bands")?;
            if let (Some(n), Some(input)) = (self.ninja, self.ninja_input()) {
                need(ninja_state(input, false) == Some(n), "the ninja follows the rule")?;
            }
        }
        if let Some(s) = run.share_pct {
            need(s <= 100, "share is a percentage")?;
        }
        need(run.words <= run.total_words, "scored words within the text")?;
        need((run.cut == CutKind::None) == (run.words == run.total_words) || self.is_paid_document(), "cut and words")?;
        need(
            self.receipt.is_some() == (self.state == DetState::Live && self.kind != Kind::Example),
            "receipt on live pages",
        )?;
        let Some(text) = self.text.as_ref() else {
            // An X page: check_x() has already required no passages.
            return Ok(());
        };
        let body_len = text.body.as_deref().map_or(0, utf16_len);
        let mut prev = 0;
        for s in &run.spans {
            need(s.start >= prev && s.start < s.end && s.end <= body_len, "passages in order within the body")?;
            need(s.words >= 24, "no passage under 24 words")?;
            prev = s.end;
        }
        for a in &text.anchors {
            need(a.start <= a.end && a.end <= body_len && a.from_word <= a.to_word, "anchors within the body")?;
        }
        need(text.body.is_some() || text.shown == Shown::Withheld, "a missing body is withheld")?;
        need((text.shown == Shown::Withheld) == text.withheld_reason.is_some(), "withheld text names its reason")?;
        if self.kind.is_secret() || matches!(self.kind, Kind::Unstored) {
            need(text.shown != Shown::Excerpt, "secret and unstored pages show the whole text")?;
        }
        if self.kind == Kind::PublicUrl {
            need(text.shown != Shown::Full, "a public page never shows the full text")?;
        }
        Ok(())
    }

    /// The X invariants: an X post is only a public page; it has no text
    /// block, passages or anchors and is always `noindex`; its source is the
    /// post (host `x.com`, the canonical post URL or the domain only, read
    /// through X, with no title, site name, author, date or page hash); an
    /// embed appears only on a live X page that is not adult, and passes
    /// [`XEmbed::check`].
    fn check_x(&self) -> Result<(), PageError> {
        if !self.is_x_post() {
            need(self.platform != Platform::X || self.kind == Kind::PublicUrl, "an X post is a public page")?;
            need(self.embed.is_none(), "an embed only on an X page")?;
            return Ok(());
        }
        need(self.robots == Robots::Noindex, "an X page is noindex")?;
        need(self.text.is_none(), "an X page has no text body or anchors")?;
        if let Some(run) = &self.run {
            need(run.spans.is_empty() && run.spans_note == Some(SpansNote::XPost), "an X page draws no passage")?;
        }
        if let Some(s) = &self.source {
            need(s.host == X_HOST && s.via == SourceVia::X, "an X page's source is the post on X")?;
            need(
                s.title.is_none()
                    && s.site_name.is_none()
                    && s.author.is_none()
                    && s.published_at.is_none()
                    && s.page_hash.is_none(),
                "an X source keeps no title, site, author, date or hash",
            )?;
            need(s.url.as_deref().is_none_or(|u| parse_x_canonical(u).is_some()), "an X source is the canonical URL")?;
        }
        if matches!(self.state, DetState::Live | DetState::ExcerptWithheld) {
            let s = self.source.as_ref().ok_or(PageError("a shown X page has its source"))?;
            need(s.url.is_some() && s.link, "a shown X page links the post")?;
        }
        if let Some(e) = &self.embed {
            need(self.state == DetState::Live, "an embed only on a live X page")?;
            need(
                self.source.as_ref().is_some_and(|s| s.reputation != Some(Reputation::Adult)),
                "an adult post has no static quote",
            )?;
            e.check().map_err(|e| PageError(e.0))?;
        }
        Ok(())
    }
}
