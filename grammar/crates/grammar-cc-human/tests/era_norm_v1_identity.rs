//! v1 form rules through the public paths that existed before they moved:
//! `era_norm`, `grammar_a_pools::{text, fineweb, splits}` and
//! `grammar_a_positives::{text, generation}`. The outputs must equal the
//! golden digests recorded from the v1 code (synthetic fixtures only).

mod form {
    pub use grammar_a_pools::fineweb::{BOILERPLATE, boilerplate, classify, list_like};
    pub use grammar_a_pools::splits::rank;
    pub use grammar_a_pools::text::{
        GAP, MAX_WORDS, MIN_WORDS, Para, Screen, ascii_letter_ratio, damaged, ends_sentence, h64, has_url, long_quotation, normalized,
        prose_reason, squash, words,
    };
    pub use grammar_a_positives::generation::doc_window_reasons as window_reasons;
    pub use grammar_a_positives::text::{sentences, words as gwords};
    pub use grammar_cc_human::era_norm::{
        ABBREVIATIONS, Choice, Conventions, DASHES, Dash, ELISIONS, ELLIPSES, EXCERPT_LAYOUT_ID, Ellipsis, KEY_SCOPE, LENGTH_WINDOW,
        LENGTH_WINDOW_ID, MIN_PARAGRAPH_WORDS, NORM_ID, PROFILE_RAW, PROFILE_V1, Prior, Profile, Quote, RANGES, Range, Site, Structure,
        counts, every_class, excerpt_layout, normalise_child, own, paragraph_screen, place_breaks, resolve, sites,
    };
}

#[path = "../../grammar-a-form/tests/support/form_v1_harness.rs"]
mod harness;

#[test]
fn era_norm_v1_outputs_match_the_golden_digests() {
    harness::check(include_str!("../../grammar-a-form/tests/support/form_v1_golden.tsv"));
}
