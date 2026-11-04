//! Synthetic fixtures for protocol A2 amendment 42's boundary break (tests
//! only; no corpus text): a hard-wrapped parent whose sentences end mid-line
//! before a long tail of parent text, and replies ending with a long word,
//! so that the edit's last line and the tail do not fit together and the
//! laid-out child takes the boundary break (item 42.1). Shared by the
//! library's test modules and the spliced binary's (by path), so it reads
//! `grammar_a_form` and the standard library only.
#![allow(dead_code)]

use grammar_a_form::conventions::{Choice, Conventions};
use grammar_a_form::profile;

const SYN: [&str; 20] = [
    "amber", "basalt", "cobalt", "delta", "ember", "fjord", "garnet", "harbor", "indigo", "juniper", "kestrel", "lantern", "meadow", "nectar", "orchid",
    "pebble", "quartz", "river", "saffron", "timber",
];
const ALT: [&str; 20] = [
    "acorn", "bramble", "canyon", "dune", "estuary", "fern", "glacier", "heather", "inlet", "jasper", "knoll", "lichen", "marsh", "nettle", "oasis",
    "prairie", "quarry", "ridge", "sorrel", "thicket",
];

/// The long last word of every reply.
pub const LONG: &str = "counterrevolutionaries";

fn prose(vocab: &[&str], seed: u64, n: usize, len: usize) -> String {
    let mut st = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
    let mut out: Vec<String> = Vec::new();
    for _ in 0..n {
        let ws: Vec<&str> = (0..len)
            .map(|_| {
                st = st.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                vocab[((st >> 33) as usize) % vocab.len()]
            })
            .collect();
        let s = ws.join(" ");
        out.push(format!("{}{}.", s[..1].to_uppercase(), &s[1..]));
    }
    out.join(" ")
}

/// Two paragraphs of 8 twelve-word sentences each, hard-wrapped at 48
/// columns (about 190 words); `seed` varies the words.
pub fn parent(seed: u64) -> String {
    let wrap = |p: &str| grammar_a_form::transfer::wrap(p, 48, 0).join("\n");
    format!("{}\n\n{}", wrap(&prose(&SYN, 2 * seed + 101, 8, 12)), wrap(&prose(&SYN, 2 * seed + 102, 8, 12)))
}

/// The bytes of `p` after `e` up to the end of its line.
pub fn tail(p: &str, e: usize) -> &str {
    &p[e..p[e..].find('\n').map_or(p.len(), |k| e + k)]
}

/// A reply of `n` words (at least 2) ending with `LONG` and a full stop.
pub fn reply(seed: u64, n: usize) -> String {
    let body = prose(&ALT, seed, 1, n - 1);
    format!("{} {LONG}.", body.trim_end_matches('.'))
}

fn none() -> Conventions {
    fn c<K>() -> Choice<K> {
        Choice { value: None, source: "none" }
    }
    Conventions { double: c(), single: c(), dash: c(), range: c(), ellipsis: c() }
}

/// Up to `n` sentence spans of `p` with a reply of the sentence's word count
/// whose laid-out child (`child_v2`, no conventions: the replies hold none of
/// their characters) takes the boundary break, with that reply.
pub fn boundary_children(p: &str, n: usize) -> Vec<((usize, usize), String)> {
    let mut out = Vec::new();
    for (k, x) in grammar_a_form::text::sentences(p).iter().enumerate() {
        if out.len() >= n {
            break;
        }
        let (s, e) = (x.start, x.end);
        if tail(p, e).len() <= 24 {
            continue;
        }
        let r = reply(301 + k as u64, grammar_a_form::text::words(&p[s..e]));
        let ch = profile::child_v2(p, Some((s, e)), &none(), &r);
        if ch.record["transfer"]["tail_boundary_breaks"].as_u64().unwrap_or(0) >= 1 {
            out.push(((s, e), r));
        }
    }
    out
}

/// Checks a laid-out single edit's final document `doc` with final span
/// `fin`, for the edit at `(s, e)` of `p`: the parent's bytes before the edit
/// are unchanged, the span ends at the reply's last word, the parent's
/// whitespace run follows it, then one line break, and the parent resumes at
/// its first non-whitespace byte, every byte after it unchanged.
pub fn check_final(p: &str, (s, e): (usize, usize), doc: &str, fin: (usize, usize)) {
    assert_eq!(&doc[..s], &p[..s]);
    let span = &doc[fin.0..fin.1];
    assert!(span.ends_with(&format!("{LONG}.")), "the span ends at the last word: {span:?}");
    assert!(!span.starts_with(char::is_whitespace) && !span.ends_with(char::is_whitespace));
    let run = p[e..].len() - p[e..].trim_start_matches(' ').len();
    assert!(run >= 1);
    assert_eq!(&doc[fin.1..fin.1 + run + 1], format!("{}\n", &p[e..e + run]), "the parent's run, then the break");
    assert!(doc.ends_with(&p[e + run..]), "the parent resumes at its first non-whitespace byte");
}
