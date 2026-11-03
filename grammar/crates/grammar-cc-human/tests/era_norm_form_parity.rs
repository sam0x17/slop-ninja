//! The sealed sentence splitter and the final-document window reasons,
//! which `grammar-a-form` holds and `grammar-a-positives` re-exports
//! (`text::sentences`) or delegates to (`generation::doc_window_reasons`):
//! these sweeps check grammar-a-positives' paths against the form crate's.
//! Synthetic strings only.
use grammar_a_form::{text as form_text, window};
use grammar_a_positives::{generation::doc_window_reasons, text as gtext};

/// Every string up to `len` characters over a small alphabet that reaches
/// each splitter branch: terminals, closers, whitespace, blank lines. Class
/// members are covered by the every-character sweep below.
fn strings(alphabet: &[char], len: usize) -> Vec<String> {
    let mut out = vec![String::new()];
    let mut frontier = vec![String::new()];
    for _ in 0..len {
        let mut next = Vec::new();
        for s in &frontier {
            for &c in alphabet {
                let mut t = s.clone();
                t.push(c);
                next.push(t);
            }
        }
        out.extend(next.iter().cloned());
        frontier = next;
    }
    out
}

fn split_form(s: &str) -> Vec<(usize, usize, bool)> {
    form_text::sentences(s).iter().map(|x| (x.start, x.end, x.punctuated)).collect()
}

fn split_positives(s: &str) -> Vec<(usize, usize, bool)> {
    gtext::sentences(s).iter().map(|x| (x.start, x.end, x.punctuated)).collect()
}

#[test]
fn era_norm_form_sentence_splitter_matches_grammar_a_positives() {
    assert_eq!(form_text::SPLITTER_ID, gtext::SPLITTER_ID);
    let alphabet = ['a', '.', '!', '"', ')', '\u{2026}', ' ', '\n', '\u{a0}'];
    let all = strings(&alphabet, 5);
    assert_eq!(all.len(), (0..=5).map(|k| alphabet.len().pow(k)).sum::<usize>());
    for s in &all {
        assert_eq!(split_form(s), split_positives(s), "{s:?}");
    }
    let prose = [
        "Dr. Smith arrived. He sat down! Did he? \u{201C}Yes.\u{201D} (Fine.) Then \u{2026} more.",
        "One line\n\nTwo line\n \t\nThree. Four?!\u{2019} Five",
        "No end at all",
        "Trailing space.  ",
        "1.5 litres. 2.0 next.Then joined.",
    ];
    for s in prose {
        assert_eq!(split_form(s), split_positives(s), "{s:?}");
    }
}

/// The alphabet sweep reaches every splitter branch but not every member of
/// the terminal, closer and whitespace classes. This sweep puts every Unicode
/// scalar value in each position those classes decide: as a terminal before
/// whitespace or the end, as a closer after a terminal before whitespace or
/// the end, and as whitespace inside a possible blank line.
#[test]
fn era_norm_form_sentence_splitter_matches_on_every_character() {
    let templates: [(&str, &str); 5] = [("a", " b"), ("a", ""), ("a.", " b"), ("a.", ""), ("a\n", "\nb")];
    let mut s = String::new();
    let mut n = 0usize;
    for c in (0..=0x10FFFFu32).filter_map(char::from_u32) {
        for (pre, post) in templates {
            s.clear();
            s.push_str(pre);
            s.push(c);
            s.push_str(post);
            assert_eq!(split_form(&s), split_positives(&s), "{s:?}");
            n += 1;
        }
    }
    assert_eq!(n, 5 * (0x110000 - 0x800));
}

#[test]
fn era_norm_form_words_match_both_word_counts() {
    let alphabet = ['a', ' ', '\n', '\t', '\u{a0}', '\u{2003}', '\u{200b}'];
    for s in strings(&alphabet, 5) {
        assert_eq!(form_text::words(&s), gtext::words(&s), "{s:?}");
        assert_eq!(form_text::words(&s), grammar_a_pools::text::words(&s), "{s:?}");
    }
}

#[test]
fn era_norm_form_window_matches_doc_window_reasons() {
    assert_eq!(window::LENGTH_WINDOW_ID, grammar_cc_human::era_norm::LENGTH_WINDOW_ID);
    for w in [window::LENGTH_WINDOW, (0, 0), (10, 20), (5, 5), (0, usize::MAX)] {
        for n in (0..=700).chain([usize::MAX - 1, usize::MAX]) {
            assert_eq!(window::reasons(n, w), doc_window_reasons(n, w), "{n} {w:?}");
        }
    }
}
