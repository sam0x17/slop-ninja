//! The renderer's one request: `POST /render {card}`, answered with the
//! card's PNG.
//!
//! The website and the secret-host Worker send the card JSON the detect
//! Worker built for the page (`POST /internal/v1/card`); this Worker trusts
//! nothing in it beyond its type: every bound and form is checked again
//! ([`slop_ninja_detect_page::Card::check`]) and every character must be one
//! the embedded fonts print.

use serde::Deserialize;
use slop_ninja_detect_page::Card;

/// The largest request body: a card's strings are bounded in bytes, so only
/// a paid document's section ladder grows with its input. 64 KiB holds a
/// ladder of about 9,300 sections of one band, or about 8,100 that
/// alternate two bands so that no neighbours merge (the most cells a card
/// draws; [`crate::routes::RENDER_WORK_BYTES`] covers it).
pub const REQUEST_BYTES: usize = 64 * 1024;

/// `POST /render`.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RenderRequest {
    pub card: Card,
}

/// The answer's `Content-Disposition` file name: a constant, never derived
/// from the page.
pub const DOWNLOAD_NAME: &str = slop_ninja_detect_page::card::DOWNLOAD_NAME;
