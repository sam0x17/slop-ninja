//! The runtime copy of the sealed sentence splitter (grammar-a-baseline
//! `full_a_spans::sentences`) equals `text::sentences` on synthetic strings
//! that stress every rule: terminal runs, closers, blank lines, CR/LF, Unicode
//! whitespace, U+001C..U+001F, emoji and combining marks.
use grammar_a_baseline::full_a_spans;
use grammar_a_positives::text;

const ALPHABET: &[&str] = &[
    "a", "Bb", "word", ".", "!", "?", "…", "\"", "'", ")", "]", "”", "’", "»", "}", "(", "“", " ", " ", "  ",
    "\n", "\n\n", "\r\n", "\r", "\t", "\u{a0}", "\u{2028}", "\u{3000}", "\u{1f}", "\u{1c}", "\u{200b}",
    "\u{301}", "🎉", "👩\u{200d}👧", "中", "。", "3:15", "e.g.", "...", "?!", " \n \n",
];

fn strings() -> Vec<String> {
    let mut state = 20261030u64;
    let mut next = |n: usize| {
        state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        ((state >> 33) as usize) % n
    };
    let mut out = vec![String::new(), " ".into(), "x".into(), ".".into(), "\n\n".into()];
    for _ in 0..20000 {
        let len = 1 + next(40);
        out.push((0..len).map(|_| ALPHABET[next(ALPHABET.len())]).collect());
    }
    out
}

#[test]
fn runtime_splitter_copy_equals_the_sealed_splitter() {
    for s in strings() {
        let a = text::sentences(&s)
            .iter()
            .map(|x| (x.start, x.end, x.punctuated))
            .collect::<Vec<_>>();
        let b = full_a_spans::sentences(&s)
            .iter()
            .map(|x| (x.start, x.end, x.punctuated))
            .collect::<Vec<_>>();
        assert_eq!(a, b, "{s:?}");
    }
    assert_eq!(text::SPLITTER_ID, full_a_spans::SPLITTER_ID);
}
