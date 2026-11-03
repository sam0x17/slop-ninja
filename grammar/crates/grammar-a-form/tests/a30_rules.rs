//! Protocol A2 amendment 30 items 30.1 and 30.2 for `cc_cohort_profile_v2`,
//! on synthetic text only (no corpus text, no live call): a closed
//! paragraph is a kept unit with both boundaries fixed; a target on it or
//! inside it is realised by the fixed boundary; the text before its start
//! is a segment of its own.
#[path = "support/a10_gen.rs"]
mod synth;

use grammar_a_form::conventions::{Choice, Conventions};
use grammar_a_form::profile::child_v2;
use grammar_a_form::text;
use grammar_a_form::transfer::{KeptKind, SourceLayout, UnitRole, transfer};
use synth::Lcg;

fn none() -> Conventions {
    fn c<K>() -> Choice<K> {
        Choice { value: None, source: "none" }
    }
    Conventions { double: c(), single: c(), dash: c(), range: c(), ellipsis: c() }
}

// ------------------------------------------------------------------ items 30.1 and 30.2

/// `n` sentences of 10 words over a fixed vocabulary, punctuated, the last
/// unpunctuated when `end` is false.
fn prose(seed: u64, n: usize, len: usize, end: bool) -> String {
    let mut r = Lcg(seed);
    let mut out = Vec::new();
    for k in 0..n {
        let ws: Vec<&str> = (0..len).map(|_| synth::VOCAB[30 + r.below(30) as usize]).collect();
        let s = ws.join(" ");
        let p = if end || k + 1 < n { "." } else { "" };
        out.push(format!("{}{}{p}", s[..1].to_uppercase(), &s[1..]));
    }
    out.join(" ")
}

/// The units drawing on raw bytes `k` (a kept unit's range).
fn drawing(t: &grammar_a_form::transfer::Transferred, k: (usize, usize)) -> Vec<(usize, usize)> {
    t.diag.units.iter().filter(|u| u.source.raw.0 < k.1 && k.0 < u.source.raw.1).map(|u| u.source.raw).collect()
}

#[test]
fn a_closed_paragraph_after_a_long_paragraph_is_its_own_unit() {
    // Count 5's shape: a long punctuated paragraph, then a closed one. The
    // source's one target falls in the prose before the closed start; the
    // closed paragraph's start is fixed, so it is never merged.
    let parent = format!("{}\n\n{}", prose(1, 1, 10, true), prose(2, 1, 30, false));
    let src = SourceLayout::whole(&parent);
    assert!(src.licence.iter().any(|r| r == "no_sentence_end"));
    let raw = format!("{}\n\n{}\n\n{}", prose(3, 1, 10, true), prose(4, 1, 10, true), prose(5, 1, 10, false));
    let t = transfer(&raw, &none(), &src);
    assert_eq!(t.record.closed_paragraphs, 1);
    let k = t.diag.kept.iter().find(|k| k.kind == KeptKind::Closed).unwrap();
    assert_eq!(drawing(&t, k.raw), vec![k.raw], "the closed paragraph shares no unit");
    assert_eq!(t.paragraphs.len(), 3);
    assert_eq!(t.layout.realised_breaks, 1);
    // its unit is a segment of its own: both edges fixed
    let segs: Vec<Option<usize>> = t.diag.units.iter().map(|u| u.segment).collect();
    assert!(segs[2].is_some() && segs[2] != segs[1], "{segs:?}");
    let c = child_v2(&parent, None, &none(), &raw);
    assert!(c.reasons.is_empty(), "{:?}", c.reasons);
}

#[test]
fn a_target_inside_a_closed_paragraph_is_realised_by_its_boundaries() {
    // Targets at a quarter (on the closed start) and at a half (inside the
    // closed paragraph): no break is placed inside it (closed_split 0).
    let parent = format!("{}\n\n{}\n\n{}", prose(11, 1, 10, true), prose(12, 1, 10, true), prose(13, 2, 10, false));
    let src = SourceLayout::whole(&parent);
    assert!(src.licence.iter().any(|r| r == "no_sentence_end"));
    assert_eq!(src.structure.fractions.len(), 2);
    let raw = format!("{}\n\n{}", prose(14, 1, 10, true), prose(15, 3, 10, false));
    let t = transfer(&raw, &none(), &src);
    let k = t.diag.kept.iter().find(|k| k.kind == KeptKind::Closed).unwrap();
    assert_eq!(drawing(&t, k.raw).len(), 1, "closed_split is 0");
    assert_eq!((t.layout.realised_breaks, t.record.targets_on_fixed_boundaries), (0, 2));
    assert_eq!(t.paragraphs.len(), 2);
}

#[test]
fn a_short_heading_before_a_closed_paragraph_stays_its_own_paragraph() {
    // Item 30.2: the text before a closed start is its own segment; under 8
    // words it is one paragraph ending at the fixed boundary, never merged
    // into the closed paragraph.
    let parent = format!("{}\n\n{}\n\n{}", prose(21, 1, 12, false), prose(22, 2, 10, true), prose(23, 1, 10, true));
    let src = SourceLayout::whole(&parent);
    assert!(src.licence.iter().any(|r| r == "no_sentence_end"));
    let raw = format!("Notes for the day\n\n{}\n\n{}", prose(24, 1, 12, false), prose(25, 2, 10, true));
    let t = transfer(&raw, &none(), &src);
    assert_eq!(t.record.closed_paragraphs, 1);
    let units: Vec<String> = t.paragraphs.iter().map(|&(a, b)| text::squash(&t.text[a..b].replace('\n', " "))).collect();
    assert_eq!(units[0], "Notes for the day", "{units:?}");
    assert_eq!(units[1], text::squash(&prose(24, 1, 12, false)));
    assert!(t.diag.units.iter().all(|u| u.role == UnitRole::Body));
    assert_ne!(t.diag.units[0].segment, t.diag.units[1].segment);
}

#[test]
fn a_closed_paragraphs_end_and_a_closed_first_paragraph_are_as_before() {
    // With no text before the closed start, its segment was the closed
    // paragraph alone before amendment 30 as well: the text after it starts
    // a new unit and takes the targets that fall in it.
    let parent = format!("{}\n\n{}\n\n{}", prose(31, 1, 10, false), prose(32, 2, 10, true), prose(33, 2, 10, true));
    let src = SourceLayout::whole(&parent);
    let raw = format!("{}\n\n{}", prose(34, 1, 10, false), prose(35, 4, 10, true));
    let t = transfer(&raw, &none(), &src);
    assert_eq!(t.record.closed_paragraphs, 1);
    let k = t.diag.kept.iter().find(|k| k.kind == KeptKind::Closed).unwrap();
    assert_eq!(drawing(&t, k.raw), vec![k.raw]);
    // the source's targets (at 0.2 and 0.6) are the closed end (on the
    // fixed boundary) and one placed in the prose after it
    assert_eq!((t.layout.realised_breaks, t.record.targets_on_fixed_boundaries), (1, 1));
    assert_eq!(t.paragraphs.len(), 3);
}
