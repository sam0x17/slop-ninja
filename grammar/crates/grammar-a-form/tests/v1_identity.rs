//! v1 byte identity: this crate's form rules reproduce the golden digests
//! recorded from the v1 code in `grammar-cc-human`, `grammar-a-pools` and
//! `grammar-a-positives` before the rules moved here. `grammar-cc-human`'s
//! `era_norm_v1_identity` test runs the same harness through the old public
//! paths. Synthetic fixtures only.

mod form {
    pub use grammar_a_form::conventions::{
        Choice, Conventions, DASHES, Dash, ELISIONS, ELLIPSES, Ellipsis, KEY_SCOPE, Prior, Quote, RANGES, Range, Site, counts, own, resolve,
        sites,
    };
    pub use grammar_a_form::layout::{ABBREVIATIONS, EXCERPT_LAYOUT_ID, MIN_PARAGRAPH_WORDS, Structure, excerpt_layout, normalise_child, place_breaks};
    pub use grammar_a_form::profile::{NORM_ID, PROFILE_RAW, PROFILE_V1, Profile, every_class};
    pub use grammar_a_form::rank::rank;
    pub use grammar_a_form::screen::{BOILERPLATE, boilerplate, classify, list_like, paragraph_screen};
    pub use grammar_a_form::text::{
        GAP, MAX_WORDS, MIN_WORDS, Para, Screen, ascii_letter_ratio, damaged, ends_sentence, h64, has_url, long_quotation, normalized,
        prose_reason, sentences, squash, words, words as gwords,
    };
    pub use grammar_a_form::window::{LENGTH_WINDOW, LENGTH_WINDOW_ID, reasons as window_reasons};
}

#[path = "support/form_v1_harness.rs"]
mod harness;

#[test]
fn v1_outputs_match_the_golden_digests() {
    harness::check(include_str!("support/form_v1_golden.tsv"));
}
