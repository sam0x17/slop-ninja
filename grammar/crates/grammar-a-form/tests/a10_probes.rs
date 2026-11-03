//! The independent checker's probes of amendment 7 (protocol A2 amendment
//! 10, problems 1-4) as regression tests, on its own synthetic generators
//! (`tests/support/a10_gen.rs`, copied from `round2/a7check/a7check.rs`).
//!
//! This file uses only APIs that exist at `ed25a148` (profile v2 as
//! amendment 7 landed it), so it compiles there too. Every test but the
//! control fails at `ed25a148` and passes under amendments 10 and 13; the
//! control pins the no-indent documents, which both read the same way.
//! Counts only; every text is synthetic.
#[path = "support/a10_gen.rs"]
mod synth;

use grammar_a_form::conventions::{Choice, Conventions};
use grammar_a_form::measure::{GapKind, LayoutClass, Measure};
use grammar_a_form::profile::child_v2;

fn none() -> Conventions {
    fn c<K>() -> Choice<K> {
        Choice { value: None, source: "none" }
    }
    Conventions { double: c(), single: c(), dash: c(), range: c(), ellipsis: c() }
}

/// The checker's tally of a probe: single breaks inside a sentence read as
/// wraps and as line breaks, documents with a W, line-form documents,
/// line-form documents with a W, and paragraphs.
#[derive(Debug, PartialEq, Eq)]
struct Tally {
    in_sentence: usize,
    wrap: usize,
    line_break: usize,
    hard_wrapped: usize,
    line_form: usize,
    line_form_with_w: usize,
    paragraphs: usize,
}

fn tally(docs: &[String]) -> Tally {
    let mut t = Tally { in_sentence: 0, wrap: 0, line_break: 0, hard_wrapped: 0, line_form: 0, line_form_with_w: 0, paragraphs: 0 };
    for d in docs {
        let m = Measure::of(d);
        for g in m.gaps.iter().filter(|g| g.in_sentence()) {
            t.in_sentence += 1;
            t.wrap += (g.kind == GapKind::Wrap) as usize;
            t.line_break += (g.kind == GapKind::LineBreak) as usize;
        }
        let c = m.counts();
        t.hard_wrapped += c.hard_wrapped as usize;
        t.line_form += (c.class == LayoutClass::LineForm) as usize;
        t.line_form_with_w += (c.class == LayoutClass::LineForm && c.hard_wrapped) as usize;
        t.paragraphs += c.paragraphs;
    }
    t
}

#[test]
fn p1_a_first_line_indent_reads_as_wrapped_prose() {
    // Problem 2 (item 10.1): widths are columns, so a greedy wrap with a
    // first-line indent passes the fit test. At ed25a148: 674 and 799 of
    // 1,670 in-sentence breaks misread; 17 and 27 documents line-form.
    for sep in ["\n\n", "\n"] {
        let t = tally(&synth::p1(4, sep));
        assert_eq!(t, Tally { in_sentence: 1670, wrap: 1670, line_break: 0, hard_wrapped: 60, line_form: 0, line_form_with_w: 0, paragraphs: 300 }, "{sep:?}");
    }
    // P1b, indent 2 at W 72 (ed25a148: 264 of 1,400 misread, one line-form).
    let t = tally(&synth::p1b());
    assert_eq!(t, Tally { in_sentence: 1400, wrap: 1400, line_break: 0, hard_wrapped: 60, line_form: 0, line_form_with_w: 0, paragraphs: 300 });
}

#[test]
fn p1_controls_without_an_indent_keep_their_reading() {
    // Control (passes at ed25a148 too): every in-sentence break is a wrap.
    // With single-newline paragraphs 17 paragraph ends whose next word would
    // have fit within the true width 64 but not within W stay read as wraps
    // (the one-W limit, unchanged).
    assert_eq!(tally(&synth::p1(0, "\n\n")), Tally { in_sentence: 1681, wrap: 1681, line_break: 0, hard_wrapped: 60, line_form: 0, line_form_with_w: 0, paragraphs: 300 });
    assert_eq!(tally(&synth::p1(0, "\n")), Tally { in_sentence: 1681, wrap: 1681, line_break: 0, hard_wrapped: 60, line_form: 0, line_form_with_w: 0, paragraphs: 283 });
}

#[test]
fn p4_unpunctuated_paragraph_ends_are_wrapped_when_the_text_has_a_w() {
    // Problem 3 (item 10.2(c), (e)). Every paragraph's last sentence is
    // unpunctuated. At ed25a148 all 60 documents were line-form and 1,080 of
    // 1,265 breaks misread.
    let docs = synth::p4();
    let t = tally(&docs);
    // The 15 documents without first-pass evidence have no W and stay
    // line-form (item 10.2's first residual limit).
    let no_w: Vec<&String> = docs.iter().filter(|d| !Measure::of(d).counts().hard_wrapped).collect();
    assert_eq!(no_w.len(), 15);
    assert!(no_w.iter().all(|d| Measure::of(d).counts().class == LayoutClass::LineForm));
    // With a W, none of the 45 is line-form (amendment 10's order of work,
    // step 3). Amendment 13 item 13.5 met that expectation: W counts every
    // line of the evidence runs, last lines included, and a line is
    // unwrapped only when it is wider than W by more than its last word and
    // one space. Under amendment 10 alone W from sparse evidence fell 1-8
    // columns below the wrap width: 6 of the 45 were line-form and 248 of
    // their in-sentence breaks line breaks. Now 2 of 947 are.
    assert_eq!(t, Tally { in_sentence: 1265, wrap: 945, line_break: 320, hard_wrapped: 45, line_form: 15, line_form_with_w: 0, paragraphs: 564 });
    let with_w: Vec<String> = docs.iter().filter(|d| Measure::of(d).counts().hard_wrapped).cloned().collect();
    assert_eq!(tally(&with_w), Tally { in_sentence: 947, wrap: 945, line_break: 2, hard_wrapped: 45, line_form: 0, line_form_with_w: 0, paragraphs: 182 });
    // P4b (only the first paragraph punctuated): every document has a W and
    // none is line-form (ed25a148: 60 of 60 line-form, 871 misread; amendment
    // 10 alone: 88 line breaks next to lines wider than W). Every break reads
    // as a wrap.
    assert_eq!(tally(&synth::p4b()), Tally { in_sentence: 1339, wrap: 1339, line_break: 0, hard_wrapped: 60, line_form: 0, line_form_with_w: 0, paragraphs: 240 });
}

#[test]
fn p4c_one_unpunctuated_closing_paragraph_no_longer_fails_the_reading() {
    // Problem 3: one unpunctuated closing paragraph of four split into
    // failing fragments (ed25a148: 510 paragraphs for 240 written, 270 line
    // breaks; amendment 10 alone: 243, three breaks next to a line wider than
    // W). Under amendment 13 item 13.5 every document reads as written.
    let docs = synth::p4c();
    let t = tally(&docs);
    assert_eq!(t, Tally { in_sentence: 1316, wrap: 1316, line_break: 0, hard_wrapped: 60, line_form: 0, line_form_with_w: 0, paragraphs: 240 });
    // The closing paragraph is one wrapped paragraph in every document.
    let whole = docs.iter().filter(|d| Measure::of(d).paragraphs.len() == 4).count();
    assert_eq!(whole, 60);
}

#[test]
fn p5_long_single_newline_paragraphs_stay_separate_and_unwrapped() {
    // Problem 4 (items 10.2(b), 10.3): block A is wrapped at 40, block B
    // holds three long single-newline paragraphs. At ed25a148 all 80 block-B
    // breaks read as wraps (80 paragraphs for 160) and mirrors were wrapped
    // at W everywhere (no child line over 40).
    let docs = synth::p5();
    let (mut b_breaks, mut b_wrap, mut paras, mut long_lines, mut first_wrapped) = (0, 0, 0, 0, 0);
    for (d, child) in &docs {
        let m = Measure::of(d);
        let k = m.gaps.iter().position(|g| g.newlines >= 2).unwrap();
        for g in &m.gaps[k + 1..] {
            b_breaks += 1;
            b_wrap += (g.kind == GapKind::Wrap) as usize;
        }
        paras += m.paragraphs.len();
        let c = child_v2(d, None, &none(), child);
        assert!(c.reasons.is_empty(), "{:?}", c.reasons);
        long_lines += c.text.lines().filter(|l| l.chars().count() > 40).count();
        first_wrapped += c.text.split("\n\n").next().unwrap().lines().all(|l| l.chars().count() <= 40) as usize;
    }
    assert_eq!((b_breaks, b_wrap, paras), (80, 0, 160));
    // A child paragraph takes the flag of the source paragraph at its
    // midpoint word fraction: the first is wrapped at W (block A's) in 38
    // children; in 2 its midpoint falls past block A's share of the words,
    // so it takes block B's unwrapped flag. Later paragraphs are unwrapped.
    assert_eq!(first_wrapped, 38);
    assert_eq!(long_lines, 119);
}

#[test]
fn p2_a_greeting_email_mirror_keeps_its_frames_and_is_screened_by_licence() {
    // Problem 1 (items 10.4-10.6). At ed25a148: 0 of 40 mirrors screened,
    // 0 rejected, the greeting merged into the first line in 40, the
    // sign-off into the last in 39, and list markers left mid-line in 20 of
    // 20 list children.
    let (mut screened, mut rejected, mut greeting_merged, mut signoff_merged, mut accepted_inline) = (0, 0, 0, 0, 0);
    for (i, (parent, child)) in synth::p2().iter().enumerate() {
        let c = child_v2(parent, None, &none(), child);
        screened += (c.record["form_screen_applied"] == true) as usize;
        rejected += (!c.reasons.is_empty()) as usize;
        let first = c.text.lines().next().unwrap_or("");
        greeting_merged += (first.starts_with("Hello Bob,") && first.split_whitespace().count() > 2) as usize;
        signoff_merged += c.text.lines().last().unwrap_or("").ends_with("regards, Sam") as usize;
        if c.reasons.is_empty() {
            accepted_inline += c.text.lines().any(|l| l.contains(" - ") || l.contains(": -")) as usize;
            // An accepted child keeps its greeting and sign-off as frame lines.
            assert_eq!(c.text.lines().next(), Some("Hello Bob,"));
            assert!(c.text.ends_with("Best regards,\nSam\n"), "{i}");
        } else {
            // The list children: the body licence is empty (the parent's body
            // passes the screen), so the list and its lead-in are rejected.
            assert_eq!(i % 2, 1);
            assert!(c.reasons.contains(&"form_line_list_like".to_string()), "{:?}", c.reasons);
        }
    }
    assert_eq!((screened, rejected, greeting_merged, signoff_merged, accepted_inline), (40, 20, 0, 0, 0));
}
