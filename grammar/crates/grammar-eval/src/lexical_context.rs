//! Shared token eligibility and lemma identity for new annotation-derived families.
//! Neither helper substitutes a surface form or tokenizes a supplied lemma.
use grammar_core::syntax::Token;
use unicode_categories::UnicodeCategories;
use unicode_normalization::UnicodeNormalization;

/// Exact original grammar-features-v1 lexical predicate, including Unicode
/// modifier letters. This is deliberately separate from word tokenization.
pub fn lexical(token: &Token) -> bool {
    !token.is_space && !token.is_punct && token.text.chars().any(|ch| ch.is_letter())
}

/// NFC, Unicode lowercase, NFC. A missing/whitespace-only lemma is None;
/// otherwise preserve all supplied characters, including punctuation and spaces.
/// Literal parser placeholders remain literal. There is no surface fallback.
pub fn normalized_lemma(token: &Token) -> Option<String> {
    if token.lemma.trim().is_empty() {
        return None;
    }
    Some(
        token
            .lemma
            .nfc()
            .collect::<String>()
            .to_lowercase()
            .nfc()
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    fn token(text: &str, lemma: &str) -> Token {
        Token {
            i: 0,
            start_byte: 0,
            end_byte: text.len(),
            text: text.into(),
            lemma: lemma.into(),
            pos: "PRON".into(),
            tag: "PRON".into(),
            dep: "ROOT".into(),
            head: 0,
            sentence: 0,
            morph: BTreeMap::new(),
            is_space: false,
            is_punct: false,
        }
    }
    #[test]
    fn preserves_exact_unicode_letter_eligibility_and_annotation_flags() {
        for text in ["we", "é", "\u{02bc}", "n't"] {
            assert!(lexical(&token(text, text)));
        }
        for text in ["123", "'", "’", "\u{0301}", " ", "🦊"] {
            assert!(!lexical(&token(text, text)));
        }
        let mut value = token("we", "we");
        value.is_space = true;
        assert!(!lexical(&value));
        value.is_space = false;
        value.is_punct = true;
        assert!(!lexical(&value));
    }
    #[test]
    fn canonical_unicode_lemmas_preserve_punctuation_and_missingness() {
        assert_eq!(
            normalized_lemma(&token("irrelevant", "E\u{301}LAN")),
            Some("élan".into())
        );
        assert_eq!(
            normalized_lemma(&token("irrelevant", "ÉLAN")),
            Some("élan".into())
        );
        assert_eq!(
            normalized_lemma(&token("irrelevant", "İ")),
            Some("i\u{0307}".into())
        );
        for (lemma, expected) in [
            ("'S", "'s"),
            ("’S", "’s"),
            ("-PRON-", "-pron-"),
            ("_", "_"),
            (" Two Words ", " two words "),
        ] {
            assert_eq!(
                normalized_lemma(&token("different-surface", lemma)),
                Some(expected.into())
            );
        }
        for lemma in ["", " \t\n", "\u{2003}"] {
            assert_eq!(
                normalized_lemma(&token("surface-does-not-fill-missing", lemma)),
                None
            );
        }
    }
}
