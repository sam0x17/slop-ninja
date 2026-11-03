//! Text-level measurements stored once per unique text.

/// Method identifiers recorded in the store metadata.
pub const WORDS_METHOD: &str = "whitespace_runs_v1";
pub const LANGUAGE_METHOD: &str = "en_stopword_share_v1";

const STOPWORDS: [&str; 48] = [
    "the", "of", "and", "to", "a", "in", "is", "that", "it", "was", "for", "on", "with", "as", "he", "be",
    "at", "by", "i", "this", "had", "not", "are", "but", "from", "or", "have", "an", "they", "which", "you",
    "were", "her", "his", "she", "all", "their", "we", "there", "been", "has", "if", "would", "so", "will",
    "what", "can", "my",
];

pub struct Stats {
    pub n_bytes: usize,
    pub n_chars: usize,
    pub n_words: usize,
    pub n_lines: usize,
    pub language: &'static str,
    pub language_score: f64,
}

/// Words are maximal runs of non-whitespace characters. The language screen is
/// the share of words that are common English function words (letters only,
/// lowercased): `en` at 0.12 or more with at least 10 words, `und` otherwise.
pub fn stats(text: &str) -> Stats {
    let mut n_words = 0;
    let mut stop = 0;
    for w in text.split_whitespace() {
        n_words += 1;
        let k: String = w.chars().filter(|c| c.is_alphabetic()).flat_map(|c| c.to_lowercase()).collect();
        if !k.is_empty() && STOPWORDS.contains(&k.as_str()) {
            stop += 1;
        }
    }
    let score = if n_words == 0 { 0.0 } else { stop as f64 / n_words as f64 };
    Stats {
        n_bytes: text.len(),
        n_chars: text.chars().count(),
        n_words,
        n_lines: if text.is_empty() { 0 } else { text.lines().count() },
        language: if n_words >= 10 && score >= 0.12 { "en" } else { "und" },
        language_score: score,
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn counts() {
        let s = super::stats("The cat sat on the mat and it was happy with the day.\nSecond line.");
        assert_eq!(s.n_words, 15);
        assert_eq!(s.n_lines, 2);
        assert_eq!(s.language, "en");
        assert_eq!(super::stats("").n_words, 0);
        assert_eq!(super::stats("zzz qqq").language, "und");
    }
}
