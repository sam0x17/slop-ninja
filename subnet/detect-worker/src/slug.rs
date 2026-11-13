//! Slugs and tokens: the contract crate's drawing, blocklist, folding and
//! paths (one source for the producer and the renderers), with the store's
//! own form test ([`crate::lookup::slug_form`]) and ids ([`crate::keys`]).

pub use slop_ninja_detect_page::slug::*;

pub use crate::keys::{manage_h, public_id_h};
pub use crate::lookup::{SlugForm, slug_form};
