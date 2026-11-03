//! The final-document length window `cc_final_length_window_v1`: the human
//! excerpt's own bound, 150..=450 whitespace words of the final (assembled)
//! document. It runs inside acceptance wherever a generator can retry, and at
//! assembly on every document of every class.
//!
//! The reason strings are those of `grammar_a_positives::generation::
//! doc_window_reasons`, which a parity test in `grammar-cc-human` pins.
use crate::text::{self, MAX_WORDS, MIN_WORDS};

pub const LENGTH_WINDOW_ID: &str = "cc_final_length_window_v1";
/// The final-document length window: the human excerpt bound (whitespace words).
pub const LENGTH_WINDOW: (usize, usize) = (MIN_WORDS, MAX_WORDS);

/// Rejection reasons for a final document of `words` whitespace words.
pub fn reasons(words: usize, window: (usize, usize)) -> Vec<String> {
    if words < window.0 {
        vec![format!("final_document_below_{}_words", window.0)]
    } else if words > window.1 {
        vec![format!("final_document_above_{}_words", window.1)]
    } else {
        vec![]
    }
}

/// `reasons` for a whole final document under `LENGTH_WINDOW`.
pub fn document_reasons(doc: &str) -> Vec<String> {
    reasons(text::words(doc), LENGTH_WINDOW)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_window_is_inclusive_at_both_ends() {
        assert_eq!(LENGTH_WINDOW, (150, 450));
        assert!(reasons(150, LENGTH_WINDOW).is_empty());
        assert!(reasons(450, LENGTH_WINDOW).is_empty());
        assert_eq!(reasons(149, LENGTH_WINDOW), vec!["final_document_below_150_words".to_string()]);
        assert_eq!(reasons(451, LENGTH_WINDOW), vec!["final_document_above_450_words".to_string()]);
        assert_eq!(reasons(5, (10, 20)), vec!["final_document_below_10_words".to_string()]);
        let doc = |n: usize| vec!["word"; n].join(" ");
        assert!(document_reasons(&doc(150)).is_empty());
        assert_eq!(document_reasons(&doc(149)).len(), 1);
        // Whitespace words: any Unicode whitespace separates, runs count once.
        assert!(document_reasons(&format!("{}\n\n\t{}", doc(100), doc(50))).is_empty());
        assert_eq!(document_reasons(&doc(451)), vec!["final_document_above_450_words".to_string()]);
    }
}
