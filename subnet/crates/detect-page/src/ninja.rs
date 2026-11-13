//! The ninja's state and the gauge's ends.
//!
//! The state is the estimated share's state clamped to what the band
//! allows. The share alone could pair a drawn sword with "Unclear evidence"
//! (a share of 75% to 90.9% sits in the unclear band, since the share never
//! exceeds the confidence) or a bow with "Very strong evidence" (one AI
//! paragraph in a long human text):
//!
//! | Share | State | | Band | Allowed |
//! |---|---|---|---|---|
//! | 75 and up | strike | | weak (no strong signs) | bow to stoic |
//! | 50 to 74 | ready | | unclear | stoic only |
//! | 25 to 49 | stoic | | strong | bow to ready |
//! | under 25 | bow | | very strong | stoic to strike |
//!
//! A short text stands ready ("Strong signs") or stoic ("No strong signs"),
//! never with the sword drawn and never bowing. A paid whole document and a
//! generic card have no state. The page JSON carries the state, so the page
//! and its card agree.

use std::ops::RangeInclusive;

use crate::codes::{Band, NinjaState, ShortResult};

/// The share's own state.
pub fn share_state(share_pct: u8) -> NinjaState {
    match share_pct {
        75.. => NinjaState::Strike,
        50.. => NinjaState::Ready,
        25.. => NinjaState::Stoic,
        _ => NinjaState::Bow,
    }
}

/// The states a band allows (`start <= end` for every band).
pub fn allowed(band: Band) -> RangeInclusive<NinjaState> {
    match band {
        Band::Weak => NinjaState::Bow..=NinjaState::Stoic,
        Band::Unclear => NinjaState::Stoic..=NinjaState::Stoic,
        Band::Strong => NinjaState::Bow..=NinjaState::Ready,
        Band::VeryStrong => NinjaState::Stoic..=NinjaState::Strike,
    }
}

/// What the state is computed from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NinjaInput {
    /// A text of 150 words and more.
    Banded { band: Band, share_pct: u8 },
    /// A short text.
    Short(ShortResult),
    /// A paid whole document.
    PaidDocument,
}

/// The ninja's state, `None` for a paid document or when cards are generic.
pub fn ninja_state(input: NinjaInput, cards_generic: bool) -> Option<NinjaState> {
    if cards_generic {
        return None;
    }
    match input {
        NinjaInput::PaidDocument => None,
        NinjaInput::Short(ShortResult::Strong) => Some(NinjaState::Ready),
        NinjaInput::Short(ShortResult::None) => Some(NinjaState::Stoic),
        NinjaInput::Banded { band, share_pct } => {
            let r = allowed(band);
            Some(Ord::clamp(share_state(share_pct), *r.start(), *r.end()))
        }
    }
}

/// How the gauge marks its two ends. All three are decorative
/// (`aria-hidden`); the gauge's `aria-label` carries the meaning. Pages and
/// cards take the same choice from the `GAUGE_ENDS` var ([`GaugeEnds::parse`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GaugeEnds {
    /// SlopNinja's own human bust at the human end and robot bust at the
    /// slop end, each clipped to a light disc with a thin mint ring
    /// (`assets/brand/gauge/`, outside the code licence like the logo).
    #[default]
    Icons,
    /// A fallback build variant: the Fluent UI emoji ninja and robot, each
    /// on a soft disc (Microsoft, MIT). A build that ships it carries the two
    /// files with Microsoft's licence beside them.
    Emoji,
    /// The fallback: "H" and "S".
    Letters,
}

impl GaugeEnds {
    /// The site path of the human end's art ([`GaugeEnds::Icons`]). The
    /// secret host carries its own copy at the same path.
    pub const HUMAN_ICON: &'static str = "/assets/gauge/human.svg";
    /// The site path of the slop end's art ([`GaugeEnds::Icons`]).
    pub const SLOP_ICON: &'static str = "/assets/gauge/robot.svg";

    /// The site path of the human end's art ([`GaugeEnds::Emoji`]). The secret
    /// host carries its own copy at the same path.
    pub const HUMAN_ASSET: &'static str = "/assets/emoji/ninja.svg";
    /// The site path of the slop end's art ([`GaugeEnds::Emoji`]).
    pub const SLOP_ASSET: &'static str = "/assets/emoji/robot.svg";

    /// The two letters of [`GaugeEnds::Letters`] (human end, slop end).
    pub const LETTERS: (&'static str, &'static str) = ("H", "S");

    /// The words the ends stand for, for the gauge's label.
    pub const WORDS: (&'static str, &'static str) = ("Human", "Slop");

    /// Every variant, in declaration order.
    pub const ALL: &'static [GaugeEnds] = &[GaugeEnds::Icons, GaugeEnds::Emoji, GaugeEnds::Letters];

    /// The `GAUGE_ENDS` value: `icons`, `emoji` or `letters` (the wire string).
    pub fn as_str(self) -> &'static str {
        match self {
            GaugeEnds::Icons => "icons",
            GaugeEnds::Emoji => "emoji",
            GaugeEnds::Letters => "letters",
        }
    }

    /// Parses a `GAUGE_ENDS` value; an absent or empty var is the default.
    pub fn parse(var: Option<&str>) -> Option<GaugeEnds> {
        match var.map(str::trim) {
            None | Some("") => Some(GaugeEnds::default()),
            Some(v) => GaugeEnds::ALL.iter().copied().find(|e| e.as_str() == v),
        }
    }
}
