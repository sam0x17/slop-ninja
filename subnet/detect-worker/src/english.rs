//! The English screen every human document of A's test passed: the human
//! pools' `text::english_screen` (`grammar/crates/grammar-a-pools`), ported
//! unchanged in its decisions and arithmetic. Latin-script letters must be
//! at least [`MIN_LATIN_LETTER_SHARE`] of all letters, and common English
//! function words at least [`MIN_ENGLISH_STOPWORD_SHARE`] of the folded
//! tokens (lowercase alphanumeric runs).
//!
//! The port allocates nothing: a folded token is compared with the function
//! words through a fixed buffer longer than the longest of them, so a token
//! that overflows it can match none. `tests/fixtures/english-screen.json`
//! holds texts and the pools' own verdicts; a test in `grammar-a-pools`
//! checks the pools' function against the same file, and a test here checks
//! the port.

/// The pools' function words (`ENGLISH_STOPWORDS`), in the same order.
pub const ENGLISH_STOPWORDS: &[&str] = &[
    "the", "of", "and", "to", "a", "in", "that", "is", "was", "he", "for", "it", "with", "as", "his", "on", "be", "at", "by",
    "i", "this", "had", "not", "are", "but", "from", "or", "have", "an", "they", "which", "you", "were", "her", "she", "there",
    "would", "their", "we", "him", "been", "has", "when", "who", "will", "no", "if", "so", "what", "its", "into", "than",
    "them", "could", "my", "me", "our", "your", "these", "those", "then", "do", "did", "any",
];
/// Minimum share of Latin-script letters among all letters.
pub const MIN_LATIN_LETTER_SHARE: f64 = 0.9;
/// Minimum share of English function words among folded tokens.
pub const MIN_ENGLISH_STOPWORD_SHARE: f64 = 0.2;

/// Longer than every function word, in chars.
const BUF: usize = 8;

/// Share of Latin-script letters (ASCII, Latin-1 Supplement, Latin
/// Extended-A/B and Latin Extended Additional) among all letters.
pub fn latin_letter_share(s: &str) -> f64 {
    let (mut l, mut all) = (0usize, 0usize);
    for c in s.chars().filter(|c| c.is_alphabetic()) {
        all += 1;
        let u = c as u32;
        if c.is_ascii_alphabetic() || (0xC0..=0x24F).contains(&u) || (0x1E00..=0x1EFF).contains(&u) {
            l += 1;
        }
    }
    if all == 0 { 0.0 } else { l as f64 / all as f64 }
}

/// A folded token being read: its first [`BUF`] lowercase chars.
struct Token {
    buf: [char; BUF],
    len: usize,
    over: bool,
}

impl Token {
    const fn new() -> Self {
        Token { buf: ['\0'; BUF], len: 0, over: false }
    }

    fn push(&mut self, c: char) {
        match self.buf.get_mut(self.len) {
            Some(slot) => {
                *slot = c;
                self.len += 1;
            }
            None => self.over = true,
        }
    }

    fn is_stopword(&self) -> bool {
        if self.over {
            return false;
        }
        let Some(chars) = self.buf.get(..self.len) else { return false };
        ENGLISH_STOPWORDS.iter().any(|w| w.chars().eq(chars.iter().copied()))
    }

    fn started(&self) -> bool {
        self.len > 0 || self.over
    }
}

/// Share of folded tokens that are common English function words.
pub fn english_stopword_share(s: &str) -> f64 {
    let (mut toks, mut stop) = (0usize, 0usize);
    let mut cur = Token::new();
    let mut end = |cur: &mut Token| {
        if cur.started() {
            toks += 1;
            if cur.is_stopword() {
                stop += 1;
            }
            *cur = Token::new();
        }
    };
    for c in s.chars() {
        if c.is_alphanumeric() {
            for l in c.to_lowercase() {
                cur.push(l);
            }
        } else {
            end(&mut cur);
        }
    }
    end(&mut cur);
    if toks == 0 { 0.0 } else { stop as f64 / toks as f64 }
}

/// The screen's verdict: `None` passes, else the pools' reason code.
pub fn english_screen(s: &str) -> Option<&'static str> {
    if latin_letter_share(s) < MIN_LATIN_LETTER_SHARE {
        return Some("latin_letter_share_below_0_90");
    }
    if english_stopword_share(s) < MIN_ENGLISH_STOPWORD_SHARE {
        return Some("english_stopword_share_below_0_20");
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_buffer_holds_every_function_word() {
        assert!(ENGLISH_STOPWORDS.iter().all(|w| w.chars().count() < BUF));
    }

    /// The pools' verdicts, recorded in the shared fixture, are the port's.
    #[test]
    fn the_port_agrees_with_the_pools_on_the_fixture() {
        let raw = include_str!("../tests/fixtures/english-screen.json");
        let v: serde_json::Value = serde_json::from_str(raw).unwrap();
        let cases = v["cases"].as_array().unwrap();
        assert!(cases.len() >= 12);
        for c in cases {
            let text = c["text"].as_str().unwrap();
            let want = c["screen"].as_str();
            assert_eq!(english_screen(text), want, "{}", c["name"]);
            let latin = c["latin_letter_share"].as_f64().unwrap();
            let stop = c["english_stopword_share"].as_f64().unwrap();
            assert_eq!(latin_letter_share(text).to_bits(), latin.to_bits(), "{}", c["name"]);
            assert_eq!(english_stopword_share(text).to_bits(), stop.to_bits(), "{}", c["name"]);
        }
    }
}
