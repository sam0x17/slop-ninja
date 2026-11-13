//! The share card's JSON (`detect-og` renders it as a 1200 x 630 PNG).
//!
//! The detect Worker builds a card from the page it describes
//! ([`Card::from_page`]), so the page and its card agree. A card never holds
//! text from the page, a rate or a "not proof" line. A secret page's card
//! holds no slug in any field: its link is [`CardLink::SecretHost`], whose
//! constructor takes no slug, so the card prints the secret host and nothing
//! after it, and a downloaded or reposted secret card leads nobody to the
//! text. Every string is bounded in bytes ([`Card::check`]) and free of control and
//! bidi characters.

use serde::{Deserialize, Serialize};

use crate::codes::*;
use crate::fmt::{iso_date, thousands};
use crate::origins::Origins;
use crate::page::Page;

/// The card schema id.
pub const CARD_SCHEMA: &str = "slop_ninja_detect_card_v1";
/// A card download's file name. Never derived from the slug.
pub const DOWNLOAD_NAME: &str = "slopninja-report.png";

/// Longest `source_label`: a DNS name in A-label form (253 octets) is the
/// longest value; the fixed labels are shorter.
pub const MAX_SOURCE_LABEL: usize = 253;
/// Longest `headline`: the paid form with two 10-digit counts.
pub const MAX_HEADLINE: usize = 72;
/// Longest `confidence_text` or `share_text` ("above 99.9%", "about 100%").
pub const MAX_FIGURE: usize = 16;
/// Longest package label in a model line.
pub const MAX_PACKAGE_LABEL: usize = 48;
/// Longest `model_line` in bytes: the label, " · " (4 bytes), 8 hex digits,
/// " · publication pending" (23 bytes).
pub const MAX_MODEL_LINE: usize = MAX_PACKAGE_LABEL + 4 + 8 + 23;
/// Longest `free_cut_note` and `document_note`: their forms with two
/// grouped 10-digit counts.
pub const MAX_NOTE: usize = 64;
/// `date` is `YYYY-MM-DD`.
pub const DATE_LEN: usize = 10;

/// The source label of a secret card.
pub const SECRET_LABEL: &str = "Private text · secret link";
/// The source label of a Bluesky post's card.
pub const BLUESKY_LABEL: &str = "a post on Bluesky";

/// Where a card points.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CardLink {
    /// A public page: prints `<site host>/d/<slug>`.
    Public { slug: String },
    /// A secret page: prints the secret host and nothing after it.
    SecretHost,
}

impl CardLink {
    /// A public page's link.
    pub fn public(slug: &str) -> CardLink {
        CardLink::Public { slug: slug.to_owned() }
    }

    /// A secret page's link. It takes no slug.
    pub fn secret_host() -> CardLink {
        CardLink::SecretHost
    }

    /// The link as the card prints it.
    pub fn printed(&self, origins: &Origins) -> String {
        match self {
            CardLink::Public { slug } => format!("{}/d/{slug}", origins.site.authority()),
            CardLink::SecretHost => origins.secret.authority().to_owned(),
        }
    }
}

/// The gauge as the card draws it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CardGauge {
    /// The needle (the share, or a paid document's share); `null` on a short
    /// text.
    pub level: Option<u8>,
    pub band: Option<Band>,
    pub short: Option<ShortResult>,
    /// The neutral paid-document dial.
    pub document: bool,
}

/// A card.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Card {
    pub schema: String,
    pub variant: CardVariant,
    pub band: Option<Band>,
    pub gauge: CardGauge,
    /// "97.3%" or "above 99.9%"; `null` on short and paid cards.
    pub confidence_text: Option<String>,
    pub share_text: Option<String>,
    pub headline: String,
    pub source_label: String,
    /// `YYYY-MM-DD`, the scan's UTC date.
    pub date: String,
    pub ninja: Option<NinjaState>,
    pub link: CardLink,
    pub model_line: String,
    /// "First 987 of 3,214 words".
    pub free_cut_note: Option<String>,
    /// "Whole document, 3,214 words, 5 sections".
    pub document_note: Option<String>,
    /// A paid document's section bands, in order (the ladder).
    #[serde(default)]
    pub ladder: Vec<Band>,
}

/// Why a page has no card.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NoCard {
    /// Not live: pending, failed, removed (451), disabled, deleted, expired.
    NotShown,
    /// The unstored result and the example reports have no card.
    Kind,
    /// The page carries no run.
    NoRun,
}

/// A broken card bound or form.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CardError(pub &'static str);

/// The headline of a band.
pub fn band_headline(band: Band) -> &'static str {
    match band {
        Band::Weak => "No strong signs of AI involvement.",
        Band::Unclear => "Unclear evidence of AI involvement.",
        Band::Strong => "Strong evidence of AI involvement.",
        Band::VeryStrong => "Very strong evidence of AI involvement.",
    }
}

/// The headline of a short text.
pub fn short_headline(r: ShortResult) -> &'static str {
    match r {
        ShortResult::Strong => "Strong signs of AI involvement.",
        ShortResult::None => "No strong signs of AI involvement.",
    }
}

/// The headline of a paid document, by sections.
pub fn document_headline(strong: u32, total: u32) -> String {
    let noun = if total == 1 { "section" } else { "sections" };
    if strong == 0 {
        format!("No strong signs of AI involvement in any of the {total} {noun}.")
    } else {
        format!("Strong signs of AI involvement in {strong} of {total} {noun}.")
    }
}

/// "First 987 of 3,214 words".
pub fn free_cut_note(words: u32, total: u32) -> String {
    format!("First {} of {} words", thousands(words.into()), thousands(total.into()))
}

/// "Whole document, 3,214 words, 5 sections".
pub fn document_note(words: u32, sections: u32) -> String {
    let noun = if sections == 1 { "section" } else { "sections" };
    format!("Whole document, {} words, {} {noun}", thousands(words.into()), thousands(sections.into()))
}

/// "span package d24 · e2536d12", with " · publication pending" while the
/// package is unpublished.
pub fn model_line(label: &str, published_digest: &str, pending: bool) -> String {
    let short = published_digest.get(..8).unwrap_or(published_digest);
    let label = clean_label(label, MAX_PACKAGE_LABEL);
    if pending { format!("{label} · {short} · publication pending") } else { format!("{label} · {short}") }
}

/// Whether `c` is a control, bidi or invisible formatting character.
pub fn is_hostile_char(c: char) -> bool {
    let u = c as u32;
    c.is_control() || matches!(u, 0xAD | 0x061C | 0x180E | 0x200B..=0x200F | 0x2028..=0x202E | 0x2060..=0x2069 | 0xFEFF)
}

/// `s` without control, bidi and invisible characters, cut to at most `max`
/// bytes on a character boundary.
pub fn clean_label(s: &str, max: usize) -> String {
    let mut out = String::with_capacity(s.len().min(max));
    for c in s.chars().filter(|c| !is_hostile_char(*c)) {
        if out.len() + c.len_utf8() > max {
            break;
        }
        out.push(c);
    }
    out
}

impl Card {
    /// The card of a live page (or a withheld excerpt, which gets the generic
    /// card). `cards_generic` is the `DETECT_CARDS = generic` switch.
    pub fn from_page(page: &Page, cards_generic: bool) -> Result<Card, NoCard> {
        if matches!(page.kind, Kind::Unstored | Kind::Example) {
            return Err(NoCard::Kind);
        }
        if !matches!(page.state, DetState::Live | DetState::ExcerptWithheld) {
            return Err(NoCard::NotShown);
        }
        let run = page.run.as_ref().ok_or(NoCard::NoRun)?;
        let link = match (page.kind, page.slug.as_deref()) {
            (Kind::PublicUrl, Some(s)) => CardLink::public(s),
            (Kind::PublicUrl, None) => return Err(NoCard::Kind),
            _ => CardLink::secret_host(),
        };
        let source_label = if page.kind.is_secret() {
            SECRET_LABEL.to_owned()
        } else if page.platform == Platform::Bluesky {
            BLUESKY_LABEL.to_owned()
        } else {
            clean_label(page.source.as_ref().map_or("", |s| s.host.as_str()), MAX_SOURCE_LABEL)
        };
        let generic = cards_generic || page.state == DetState::ExcerptWithheld;
        let free_cut_note = (run.cut != CutKind::None).then(|| free_cut_note(run.words, run.total_words));
        let model_line = model_line(&run.package.label, &run.package.published_digest, run.package.publication_pending);
        let date = iso_date(run.scored_at);
        if let Some(s) = page.sections.as_ref().filter(|_| page.is_paid_document()) {
            return Ok(Card {
                schema: CARD_SCHEMA.into(),
                variant: CardVariant::PaidDocument,
                band: None,
                gauge: CardGauge { level: Some(s.document.share_pct), band: None, short: None, document: true },
                confidence_text: None,
                share_text: Some(s.document.share_text.clone()),
                headline: document_headline(s.document.strong_sections, s.document.total_sections),
                source_label,
                date,
                ninja: None,
                link,
                model_line,
                free_cut_note: None,
                document_note: Some(document_note(run.total_words, s.document.total_sections)),
                ladder: s.items.iter().map(|x| x.band).collect(),
            });
        }
        let (variant, headline) = match (run.band, run.short) {
            (None, Some(r)) => (CardVariant::Short, short_headline(r)),
            (Some(b), None) => (if generic { CardVariant::Generic } else { CardVariant::Band }, band_headline(b)),
            _ => return Err(NoCard::NoRun),
        };
        Ok(Card {
            schema: CARD_SCHEMA.into(),
            variant,
            band: run.band,
            gauge: CardGauge { level: run.share_pct, band: run.band, short: run.short, document: false },
            confidence_text: run.confidence_text.clone(),
            share_text: run.share_text.clone(),
            headline: headline.to_owned(),
            source_label,
            date,
            ninja: if generic { None } else { page.ninja },
            link,
            model_line,
            free_cut_note,
            document_note: None,
            ladder: Vec::new(),
        })
    }

    /// Checks every bound and form.
    pub fn check(&self) -> Result<(), CardError> {
        let need = |ok: bool, what: &'static str| if ok { Ok(()) } else { Err(CardError(what)) };
        need(self.schema == CARD_SCHEMA, "schema")?;
        let clean = |s: &str, max: usize| s.len() <= max && !s.chars().any(is_hostile_char);
        need(clean(&self.source_label, MAX_SOURCE_LABEL), "source_label")?;
        need(clean(&self.headline, MAX_HEADLINE) && !self.headline.is_empty(), "headline")?;
        need(clean(&self.model_line, MAX_MODEL_LINE), "model_line")?;
        for f in [&self.confidence_text, &self.share_text].into_iter().flatten() {
            need(clean(f, MAX_FIGURE), "figure")?;
        }
        for n in [&self.free_cut_note, &self.document_note].into_iter().flatten() {
            need(clean(n, MAX_NOTE), "note")?;
        }
        need(
            self.date.len() == DATE_LEN
                && self
                    .date
                    .bytes()
                    .enumerate()
                    .all(|(i, b)| if i == 4 || i == 7 { b == b'-' } else { b.is_ascii_digit() }),
            "date",
        )?;
        if let CardLink::Public { slug } = &self.link {
            need(crate::slug::is_canonical(slug, crate::slug::PUBLIC_LEN), "public link slug")?;
        }
        match self.variant {
            CardVariant::Band => {
                need(self.band.is_some() && self.gauge.short.is_none() && self.gauge.level.is_some(), "band card")?
            }
            CardVariant::Generic => need(self.band.is_some() && self.ninja.is_none(), "generic card")?,
            CardVariant::Short => need(
                self.gauge.short.is_some()
                    && self.band.is_none()
                    && self.confidence_text.is_none()
                    && self.share_text.is_none(),
                "short card",
            )?,
            CardVariant::PaidDocument => need(
                self.gauge.document && self.ninja.is_none() && !self.ladder.is_empty() && self.document_note.is_some(),
                "paid card",
            )?,
        }
        need(self.gauge.level.is_none_or(|l| l <= 100), "gauge level")?;
        Ok(())
    }
}
