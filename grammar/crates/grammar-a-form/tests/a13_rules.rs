//! Protocol A2 amendment 13 (the independent review of amendment 10) for
//! `cc_cohort_profile_v2`, on synthetic text only: the licence screen reads
//! a child's final paragraphs by construction (item 13.1); the fit bound
//! from candidate runs (13.2); the first pass's list test (13.3); the
//! greeting retest (13.4); W and unwrapped lines (13.5); a mixed child's
//! last-line budget (13.7); edge-frame separators and the child frame rule
//! (13.8); the counts item 13.10 adds; and exact break targets (a code fix
//! under amendment 10 item 10.6(b)).
#[path = "support/a10_gen.rs"]
mod synth;

use grammar_a_form::conventions::{Choice, Conventions};
use grammar_a_form::measure::{GapKind, LayoutClass, Measure};
use grammar_a_form::profile::{EditV2, SCREEN_READING, child_v2, item_v2};
use grammar_a_form::screen::list_like;
use grammar_a_form::text;
use grammar_a_form::transfer::{SourceLayout, transfer, wrap};
use synth::Lcg;

fn none() -> Conventions {
    fn c<K>() -> Choice<K> {
        Choice { value: None, source: "none" }
    }
    Conventions { double: c(), single: c(), dash: c(), range: c(), ellipsis: c() }
}

/// The byte span of whitespace words `a..b` of `t`.
fn word_span(t: &str, a: usize, b: usize) -> (usize, usize) {
    let mut spans = Vec::new();
    let mut start = None;
    for (i, c) in t.char_indices() {
        if c.is_whitespace() {
            if let Some(s) = start.take() {
                spans.push((s, i));
            }
        } else if start.is_none() {
            start = Some(i);
        }
    }
    if let Some(s) = start {
        spans.push((s, t.len()));
    }
    (spans[a].0, spans[b - 1].1)
}

/// A parent of three paragraphs of four sentences, greedy-wrapped at 66,
/// and the word ranges of the second paragraph's sentences.
fn wrapped_parent(r: &mut Lcg) -> (String, Vec<(usize, usize)>) {
    let mut out = Vec::new();
    let mut ranges = Vec::new();
    let mut words = 0;
    for p in 0..3 {
        let sents: Vec<String> = (0..4).map(|_| synth::sentence(r, true)).collect();
        for s in &sents {
            let n = s.split_whitespace().count();
            if p == 1 {
                ranges.push((words, words + n));
            }
            words += n;
        }
        out.push(wrap(&sents.join(" "), 66, 0).join("\n"));
    }
    (out.join("\n\n"), ranges)
}

#[test]
fn mixed_children_in_hard_wrapped_parents_are_screened_by_construction() {
    // The review's F10: one sentence inside paragraph 2 replaced by a new
    // sentence, or a new sentence inserted after it. The parent's text after
    // the span is human text and is never re-wrapped, so a re-measurement
    // splits the paragraph at the splice; amendment 10's screen re-measured
    // and rejected most of these children (no_sentence_end on the fragment).
    // Read by construction, the span's paragraph is the parent's paragraph
    // with the generated text in it, and they pass.
    let mut r = Lcg(1301);
    let (mut n, mut differs, mut would_reject) = (0, 0, 0);
    for _ in 0..40 {
        let (parent, ranges) = wrapped_parent(&mut r);
        let (a, b) = ranges[1 + r.below(2) as usize];
        let sp = word_span(&parent, a, b);
        let raw = synth::sentence(&mut r, true);
        let c = child_v2(&parent, Some(sp), &none(), &raw);
        assert!(c.reasons.is_empty(), "{:?}", c.reasons);
        assert_eq!(c.record["form_screen_reading"], SCREEN_READING);
        differs += (c.record["remeasure"]["differs"] == true) as usize;
        would_reject += (!c.record["remeasure"]["form_reasons"].as_array().unwrap().is_empty()) as usize;
        // An insertion after the sentence, separator " ".
        let ins = item_v2(&parent, &none(), &[EditV2 { span: (sp.1, sp.1), insert_sep: Some(" "), raw: &raw }]);
        assert!(ins.reasons.is_empty(), "{:?}", ins.reasons);
        differs += (ins.record["edits"][0]["remeasure"]["differs"] == true) as usize;
        // Two edits in the same parent paragraph: each is screened on the
        // constructed paragraph holding its text.
        let (c0, c1) = ranges[0];
        let (d0, d1) = ranges[3];
        let two = item_v2(&parent, &none(), &[
            EditV2 { span: word_span(&parent, c0, c1), insert_sep: None, raw: &raw },
            EditV2 { span: word_span(&parent, d0, d1), insert_sep: None, raw: "The office will send the figures for the second quarter next week." },
        ]);
        assert!(two.reasons.is_empty(), "{:?}", two.reasons);
        n += 1;
    }
    assert_eq!(n, 40);
    // The re-measured readings differ in most children, and amendment 10's
    // screen would have rejected many (counts recorded, never a verdict).
    assert!(differs > 40 && would_reject > 10, "{differs} {would_reject}");
}

#[test]
fn construction_still_rejects_what_the_licence_does_not_cover() {
    let mut r = Lcg(1302);
    let (parent, ranges) = wrapped_parent(&mut r);
    let sp = word_span(&parent, ranges[1].0, ranges[1].1);
    // A replacement cut off mid-sentence: the line test and the constructed
    // paragraph's no_sentence_end only when the paragraph then ends
    // unpunctuated; here the line test rejects it.
    let cut = child_v2(&parent, Some(sp), &none(), "The office will send the figures for the");
    assert!(cut.reasons.contains(&"form_line_no_sentence_end".to_string()), "{:?}", cut.reasons);
    // A list line is unlicensed in a prose paragraph.
    let lst = child_v2(&parent, Some(sp), &none(), "- the figures for the second quarter.");
    assert!(lst.reasons.contains(&"form_line_list_like".to_string()), "{:?}", lst.reasons);
    // A span ending the parent's last paragraph with an unpunctuated
    // replacement: the constructed paragraph ends unpunctuated.
    let end = parent.trim_end().len();
    let last_sent = parent[..end - 1].rfind(". ").map_or(0, |k| k + 2);
    let tail = child_v2(&parent, Some((last_sent, end)), &none(), "and the bridge will open in the spring");
    assert!(tail.reasons.iter().any(|x| x.contains("no_sentence_end")), "{:?}", tail.reasons);
}

#[test]
fn on_excerpt_layout_parents_construction_equals_re_measurement() {
    // Item 7.7's equivalence: on a CC excerpt-layout parent the constructed
    // paragraphs are the re-measured ones, so the verdicts are v1's.
    let parent = "The ferry left the harbour at dawn and the gulls followed it out to sea. By noon the wind had dropped.\n\nThe crossing took four hours and nobody was sick for once this year at all.\n\nThe passengers came up on deck to eat their lunch in the sun.";
    let s = parent.find("By noon").unwrap();
    let e = parent.find("this year").unwrap() + "this year".len();
    let c = child_v2(parent, Some((s, e)), &none(), "The wind fell away by noon.\n\nThe crossing was calm and quick for once");
    assert_eq!(c.record["remeasure"]["differs"], false);
    let m = child_v2(parent, None, &none(), "The ferry sailed at first light with the gulls behind it.\n\nThe crossing took four hours and it was calm all the way across.\n\nEveryone ate lunch up on the deck in the warm sun.");
    assert!(m.reasons.is_empty(), "{:?}", m.reasons);
    assert_eq!(m.record["remeasure"]["differs"], false);
}

#[test]
fn a_mixed_childs_last_line_fits_with_the_parent_text_after_it() {
    // Item 13.7: a span ending mid-line; the replacement's last line and the
    // parent text after the span on that line fit within W.
    let mut r = Lcg(1303);
    let mut splits = 0;
    for _ in 0..40 {
        let (parent, ranges) = wrapped_parent(&mut r);
        let (a, b) = ranges[1];
        let sp = word_span(&parent, a, b);
        let src = SourceLayout::span(&parent, sp.0, sp.1).unwrap();
        let w = src.width.unwrap();
        let line_end = parent[sp.1..].find('\n').map_or(parent.len(), |k| sp.1 + k);
        assert_eq!(src.tail_column, parent[sp.1..line_end].trim_end().chars().count());
        let raw = format!("{} {}", synth::sentence(&mut r, true), synth::sentence(&mut r, true));
        let t = transfer(&raw, &none(), &src);
        splits += t.record.tail_budget_splits;
        let last = t.text.lines().last().unwrap();
        if last.contains(' ') {
            let start = if t.text.contains('\n') { last.chars().count() - last.trim_start().chars().count() } else { src.column };
            assert!(start + last.trim_start().chars().count() + src.tail_column <= w, "{last:?}");
        }
    }
    assert!(splits > 0);
}

#[test]
fn a_greeting_on_a_single_newline_no_longer_hides_the_wrap() {
    // Item 13.4: "Bob," then a single newline then a body whose only
    // candidate run starts at the greeting. The run's first break fails the
    // fit; the greeting starts its block and is short, so the run is tested
    // from its second line against the whole text's bound: W is found and
    // the greeting stays its own paragraph (amendment 10: no W, the body's
    // breaks were line breaks and the email line-form).
    let body = wrap("The committee met on Monday to discuss the plan for the harbour and the new bridge, and agreed the work should start in the spring.", 60, 0).join("\n");
    assert_eq!(body.lines().count(), 3);
    let t = format!("Bob,\n{body}\n\nThanks,\nSam");
    let m = Measure::of(&t);
    assert_eq!(m.width, Some(body.lines().map(|l| l.chars().count()).max().unwrap()));
    assert_eq!(m.gaps.iter().map(|g| g.kind).collect::<Vec<_>>(), vec![GapKind::LineBreak, GapKind::Wrap, GapKind::Wrap, GapKind::BlankRun, GapKind::LineBreak]);
    assert_eq!((m.diag.retested_runs, m.diag.retested_runs_evidence), (1, 1));
    assert_eq!(m.frames().head, 1);
    // A two-line body too.
    let two = wrap("The committee met on Monday to discuss the plan for the harbour and agreed it.", 60, 0).join("\n");
    assert_eq!(two.lines().count(), 2);
    assert!(Measure::of(&format!("Bob,\n{two}\n\nThanks,\nSam")).hard_wrapped);
    // Not for a line inside a block (an IRC-like line after another), nor
    // when the remainder fails the whole text's bound: a short three-line
    // signature under a body wrapped at 66 stays three paragraphs.
    let irc = "[12:01] <ann> anyone around to help with the build?\n[12:02] <bob> yes, what broke\n[12:02] <ann> the linker fails on the second step every time\n[12:03] <bob> did you clear the cache first?";
    assert!(!Measure::of(irc).hard_wrapped);
    let sig = format!("{}\n\nJohn Smith\nSenior Manager, Gas Trading\nNorth Harbour Holdings Corp.", wrap("The committee met on Monday to discuss the plan for the harbour and the new bridge, and agreed that the work should start in the spring of next year when the weather is better.", 66, 0).join("\n"));
    let ms = Measure::of(&sig);
    let n = ms.gaps.len();
    assert_eq!(ms.gaps[n - 2..].iter().map(|g| g.kind).collect::<Vec<_>>(), vec![GapKind::LineBreak, GapKind::LineBreak]);
}

#[test]
fn line_form_counts_both_wrap_kinds() {
    // Item 13.9's count: lines in failing paragraphs that hold no first-pass
    // evidence wrap. Two wrapped paragraphs ending unpunctuated hold only
    // second-pass wraps: they count here, not in the line-form test.
    let p34 = "aaaa bbbb cccc dddd eeee ffff gggg\nhhhh iiii jjjj kkkk llll mmmm nnnn.";
    let t = format!("{p34}\n\noooo pppp qqqq rrrr ssss tttt uuuu\nvvvv wwww xxxx yyyy zzzz aaaa bbbb\n\ncccc dddd eeee ffff gggg hhhh iiii\njjjj kkkk llll mmmm nnnn oooo pppp");
    let c = Measure::of(&t).counts();
    assert_eq!((c.class, c.line_form_fail_lines, c.line_form_fail_lines_evidence, c.lines), (LayoutClass::HardWrapped, 0, 4, 6));
}

/// `n` words ending a sentence.
fn words(n: usize, k: usize) -> String {
    let w: Vec<&str> = (0..n).map(|i| synth::VOCAB[(i * 7 + k) % synth::VOCAB.len()]).collect();
    format!("{}.", w.join(" "))
}

#[test]
fn break_targets_on_fixed_boundaries_are_exact() {
    // Amendment 10 item 10.6(b): a target on a fixed boundary is realised by
    // it. Amendment 10's code placed targets in floating point, so for some
    // word counts (cum / total) * total fell just inside the prose segment
    // and an extra break was placed there (the review's F15). Positions are
    // now compared in integer arithmetic.
    let text_of = |a: usize, b: usize| format!("{}\n\n- one two three four\n- five six seven eight\n\n{}", words(a, 0), words(b, 3));
    // The first target sits on the boundary after the first paragraph: its
    // floating position must fall inside that paragraph.
    let pair = (8..60).flat_map(|a| (8..60).map(move |b| (a, b))).find(|&(a, b)| {
        let w: Vec<usize> = SourceLayout::whole(&text_of(a, b)).body_wrap.iter().map(|x| x.0).collect();
        let total: usize = w.iter().sum();
        (w[0] as f64 / total as f64) * (total as f64) < w[0] as f64
    });
    let (a, b) = pair.expect("a pair that does not round-trip");
    let src_text = text_of(a, b);
    let src = SourceLayout::whole(&src_text);
    assert!(!src.line_form && src.licence.iter().any(|r| r == "list_marker"));
    let t = transfer(&src_text, &none(), &src);
    assert_eq!(t.layout.target_breaks, 3);
    assert_eq!((t.record.targets_on_fixed_boundaries, t.layout.realised_breaks, t.record.kept_list_lines), (3, 0, 2));
    assert_eq!(t.paragraphs.len(), 4);
    let squash = |s: &str| s.lines().map(text::squash).filter(|l| !l.is_empty()).collect::<Vec<_>>();
    assert_eq!(squash(&t.text), squash(&src_text));
}

#[test]
fn closed_and_kept_units_stay_units() {
    // Item 13.10's stop invariant "a kept unit merged into prose" is read on
    // the transfer's units: each closed unpunctuated paragraph and each kept
    // list line is its own unit, whatever a re-measurement reads.
    let p = |s: &str| wrap(s, 60, 0).join("\n");
    let src_text = format!(
        "{}\n{}\n{}",
        p("The committee met on Monday to discuss the plan for the harbour and agreed the work should start soon."),
        p("they also asked the office to write to every family in the village about the closures and the diversions"),
        p("The vote was postponed until next month when the full report on the harbour will be ready.")
    );
    let src = SourceLayout::whole(&src_text);
    assert!(src.licence.iter().any(|r| r == "no_sentence_end"));
    let child = "The council met on Tuesday and talked about the new parking plan for the town centre.\n\nthe office wrote to every family in the town about the bus changes and the parking rules near the school\n\nThe report will be ready next month and everyone will be able to read it then.";
    let t = transfer(child, &none(), &src);
    assert_eq!(t.record.closed_paragraphs, 1);
    let units: Vec<&str> = t.paragraphs.iter().map(|&(a, b)| &t.text[a..b]).collect();
    assert!(units.iter().any(|u| text::squash(u).ends_with("near the school")), "{units:?}");
    let c = child_v2(&src_text, None, &none(), child);
    assert!(c.reasons.is_empty(), "{:?}", c.reasons);
    assert!(c.record["remeasure"].is_object());
    // No list line of an accepted child shares a unit with another line.
    for u in units {
        let ls: Vec<&str> = u.lines().collect();
        assert!(ls.len() == 1 || !ls.iter().any(|l| list_like(l)));
    }
}
