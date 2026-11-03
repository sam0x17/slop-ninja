//! Protocol A2 amendment 10's rules for `cc_cohort_profile_v2`, on synthetic
//! text only: widths as columns (item 10.1), the first and second passes,
//! unwrapped lines and lead changes (10.2), wrap flags (10.3), edge frames
//! (10.4), the licence screen `cc_form_licence_screen_v1` (10.5), kept units
//! and whitespace by role (10.6), the ids (10.7) and the closed population
//! list (10.8); the counterexamples item 10.2 and the order of work name;
//! the design-B prototype's adversarial generators and hand cases; and the
//! stop invariants of item 10.12 over synthetic corpora. Pins changed by
//! amendment 13 (the review of amendment 10) say so where they change.
#[path = "support/a10_gen.rs"]
mod synth;

use grammar_a_form::conventions::{Choice, Conventions};
use grammar_a_form::layout::{MIN_PARAGRAPH_WORDS, Structure, normalise_child, rebuild_child};
use grammar_a_form::measure::{Frames, GapKind, LayoutClass, MEASURE_ID, Measure, WidthRule};
use grammar_a_form::profile::{EditV2, PROFILE_V2, Profile, R2_POPULATIONS, child_v2, donor_child_v2, is_r2_population, item_v2, norm_prior_seed};
use grammar_a_form::screen::{FORM_LICENCE_SCREEN_ID, LIST_MARKER, QUOTED_LINE, classify, classify_all, list_like, v1_reason};
use grammar_a_form::text;
use grammar_a_form::transfer::{LAYOUT_TRANSFER_ID, Path, SourceLayout, transfer, wrap};
use GapKind::{BlankRun as B, LineBreak as L, SentenceBreak as S, Wrap as W};

fn none() -> Conventions {
    fn c<K>() -> Choice<K> {
        Choice { value: None, source: "none" }
    }
    Conventions { double: c(), single: c(), dash: c(), range: c(), ellipsis: c() }
}

fn kinds(t: &str) -> Vec<GapKind> {
    Measure::of(t).gaps.iter().map(|g| g.kind).collect()
}

fn reasons_of(c: &grammar_a_form::profile::ChildV2) -> Vec<String> {
    c.reasons.clone()
}

/// A wrapped body: two paragraphs greedy-wrapped at `w`, blank-line apart.
fn body(w: usize) -> String {
    let p1 = "The committee met on Monday to discuss the plan for the harbour and the new bridge, and agreed that the work should start in the spring of next year when the weather is better.";
    let p2 = "They also asked the office to write to every family in the village about the road closures, the bus diversions and the new parking rules near the school gates.";
    format!("{}\n\n{}", wrap(p1, w, 0).join("\n"), wrap(p2, w, 0).join("\n"))
}

// ------------------------------------------------------------------ 10.1

#[test]
fn widths_are_columns_of_lead_and_body() {
    // Indented lines: W counts the lead. A tab is one column and trailing
    // whitespace is not counted.
    let t = "  We were glad to hear that the hall is free on the\n  first Saturday of the month, and we would like to\n  book it for the whole afternoon if that is still\n  possible. Please let us know the cost by Friday.   ";
    let m = Measure::of(t);
    assert_eq!(m.width, Some(51));
    assert_eq!(m.columns(0), 51);
    assert_eq!(Measure::of_rule(t, WidthRule::Body).width, Some(49));
    assert_eq!(Measure::of_rule(t, WidthRule::Body).diag.width_rule, WidthRule::Body);
    let tab = "\tAaaa bbbb cc dd\n\tee ff gg hh ii j.";
    assert_eq!(Measure::of(tab).columns(0), 16);
    assert_eq!(Measure::of(tab).diag.tab_led_lines, 2);
    // A first-line indent the wrapper counted passes the fit test with
    // columns and fails it with bodies (problem 2).
    let ind = "    The quick brown fox jumps over\ndog and the cat ran far away fast.";
    assert_eq!(kinds(ind), vec![W]);
    assert_eq!(Measure::of(ind).width, Some(34));
    assert_eq!(Measure::of_rule(ind, WidthRule::Body).gaps[0].kind, L);
}

#[test]
fn child_budgets_are_w_less_the_lead_taken_and_the_column() {
    // A source wrapped at 40 with every line indented two spaces: W = 42
    // columns; each child line holds 40 body characters at most.
    let p = "The committee met on Monday to discuss the plan for the harbour and the new bridge, and agreed that the work should start in the spring of next year.";
    let src_text = wrap(p, 40, 0).iter().map(|l| format!("  {l}")).collect::<Vec<_>>().join("\n");
    let src = SourceLayout::whole(&src_text);
    let w = src.width.unwrap();
    assert_eq!(w, 2 + wrap(p, 40, 0).iter().map(|l| l.chars().count()).max().unwrap());
    let child = "The council members gathered on Tuesday evening to review the proposal for the waterfront and the replacement bridge and decided the work could begin early.";
    let t = transfer(child, &none(), &src);
    for l in t.text.lines() {
        assert!(l.starts_with("  "), "{l:?}");
        assert!(l.chars().count() <= w, "{l:?}");
    }
    assert_eq!(t.text.lines().map(|l| l.trim_start().to_string()).collect::<Vec<_>>(), wrap(child, w - 2, 0));
    // A mixed child's first line: W less its column (lead and body before
    // the span).
    let s = src_text.find("discuss").unwrap();
    let e = src_text.find("next year.").unwrap() + "next year.".len();
    let sp = SourceLayout::span(&src_text, s, e).unwrap();
    let line0 = src_text.lines().find(|l| l.contains("discuss")).unwrap();
    assert_eq!(sp.column, line0.find("discuss").unwrap());
    let c = transfer(child, &none(), &sp);
    let first = c.text.lines().next().unwrap();
    assert!(sp.column + first.chars().count() <= w);
    assert!(sp.column + first.chars().count() + 1 + c.text.lines().nth(1).unwrap().trim_start().split(' ').next().unwrap().chars().count() > w);
    // A word longer than its budget stands alone.
    assert_eq!(wrap("a verylongwordthatdoesnotfit b", 10, 8), vec!["a", "verylongwordthatdoesnotfit", "b"]);
}

// ------------------------------------------------------------------ 10.2

#[test]
fn the_first_pass_list_test_and_w() {
    // Amendment 13 item 13.3: a run is list-like when its first line is
    // list_like or two consecutive lines of it are. A list_like line after a
    // break inside a sentence with no list_like neighbour is a continuation
    // (a wrapped "--", dash or number): the run passes the fit (27 + 1 + 1
    // > 27), ends a sentence and holds 12 words, so it gives evidence.
    // Amendment 10 tested every line and barred it.
    let fit = "Aaaa bbbb cc dd ee ff gg hh\n- ii jj kk.";
    assert_eq!(kinds(fit), vec![W]);
    assert_eq!((Measure::of(fit).diag.rejected_list_like, Measure::of(fit).diag.evidence_runs_with_inner_list_like), (0, 1));
    for lead in ["-- ii jj kk.", "\u{2014} ii jj kk.", "3.5 ii jj kk."] {
        assert_eq!(kinds(&format!("Aaaa bbbb cc dd ee ff gg hh\n{lead}")), vec![W], "{lead:?}");
    }
    // Two consecutive list_like lines bar the run, and so does a list_like
    // first line.
    let two = "Aaaa bbbb cc dd ee ff gg hh\n- ii jj kk ll\n- mm nn.";
    assert_eq!(kinds(two), vec![L, L]);
    assert_eq!(Measure::of(two).diag.rejected_list_like, 1);
    assert_eq!(kinds("- Aaaa bbbb cc dd ee ff gg\nhh ii jj kk ll."), vec![L]);
    // A run that fails the fit is not evidence whatever its lines.
    assert_eq!(kinds("Aaaa bbbb cc dd ee\n- ff gg hh ii jj kk."), vec![L]);
    assert_eq!(kinds("Aaaa bbbb cc dd ee ff gg hh\nii jj kk."), vec![W]);
    // W is the widest line of the evidence runs, their last lines included
    // (item 13.5), and the second pass never sets it: here the only
    // first-pass evidence is in the first block.
    let x = format!("{}\n\nshort unpunctuated line here and there and\nanother", body(50));
    let m = Measure::of(&x);
    let k = m.lines.iter().position(|l| x[l.start..].starts_with("short")).unwrap();
    let evidence_max = (0..k).filter(|&i| m.body(i).contains(' ')).map(|i| m.columns(i)).max();
    assert_eq!(m.width, evidence_max);
}

#[test]
fn a_long_line_in_the_block_no_longer_bars_the_fit() {
    // Amendment 13 item 13.2: the fit bound is the widest line holding more
    // than one word among the lines of the block's candidate runs. One long
    // unwrapped sentence line (a single-line run) or a long one-word line in
    // the same block as single-newline paragraphs wrapped at 50 left
    // amendment 10 with no W; now W is the wrap width and the long line is
    // unwrapped (or, one word, stands alone).
    let p = |s: &str| wrap(s, 50, 0).join("\n");
    let a = p("The committee met on Monday to discuss the plan for the harbour and the new bridge, and agreed the work should start soon.");
    let b = p("They also asked the office to write to every family in the village about the road closures and the bus diversions.");
    let long = "Most people agreed with the plan, although some of the shop owners were worried about the loss of trade in the spring and summer.";
    for t in [format!("{long}\n{a}\n{b}"), format!("{a}\n{long}\n{b}"), format!("{a}\n{b}\n{long}")] {
        let m = Measure::of(&t);
        assert!(m.width.is_some_and(|w| w <= 50), "{:?}", m.width);
        let k = m.lines.iter().position(|l| t[l.body.0..l.body.1] == *long).unwrap();
        assert!(m.unwrapped[k]);
        assert_eq!(m.diag.blocks_widest_outside_runs, 1);
        assert_eq!(m.paragraphs.len(), 3, "{t:?}");
        assert!(m.paragraphs.iter().filter(|p| !p.unwrapped_line).all(|p| p.wrapped));
    }
    let url = format!("{a}\nhttp://example.org/a/very/long/path/that/goes/on/and/on/for/ever/and/ever\n{b}");
    assert!(Measure::of(&url).width.is_some_and(|w| w <= 50));
}

#[test]
fn unwrapped_lines_and_their_breaks() {
    // A block wrapped at 40, then single-newline paragraphs of long lines:
    // the long lines are unwrapped (no evidence run, wider than W, several
    // words), so their sentence-end breaks are boundaries (problem 4).
    let a = wrap("The committee met on Monday to discuss the plan for the harbour and the new bridge, and agreed the work should start soon.", 40, 0).join("\n");
    let b1 = "They also asked the office to write to every family in the village about the road closures and the bus diversions.";
    let b2 = "Most people agreed with the plan, although some of the shop owners were worried about the loss of trade in the spring.";
    let t = format!("{a}\n\n{b1}\n{b2}");
    let m = Measure::of(&t);
    let n = m.lines.len();
    assert_eq!(m.unwrapped[n - 2..], [true, true]);
    assert_eq!(m.gaps[n - 2].kind, S);
    assert_eq!(m.paragraphs.len(), 3);
    assert!(m.paragraphs[1].unwrapped_line && !m.paragraphs[0].unwrapped_line);
    assert_eq!((m.diag.unwrapped_lines, m.diag.unwrapped_paragraphs, m.diag.breaks_next_to_unwrapped), (2, 2, 1));
    // A one-word line wider than W is not unwrapped.
    let one = format!("{a}\n\n{}", "x".repeat(60));
    assert!(!Measure::of(&one).unwrapped.iter().any(|&u| u));
}

/// A punctuated two-line paragraph whose lines are 34 columns: W 34.
const P34: &str = "aaaa bbbb cccc dddd eeee ffff gggg\nhhhh iiii jjjj kkkk llll mmmm nnnn.";

#[test]
fn the_second_pass_reads_wraps_from_w() {
    // An unpunctuated wrapped paragraph after a punctuated one: its run
    // fails the first pass (no sentence end) and the second pass wraps it.
    let t = format!("{P34}\n\noooo pppp qqqq rrrr ssss tttt uuuu\nvvvv wwww xxxx yyyy zzzz aaaa bbbb");
    let m = Measure::of(&t);
    // W 35: the evidence run's last line counts (amendment 13 item 13.5).
    assert_eq!(m.width, Some(35));
    assert_eq!(kinds(&t), vec![W, B, W]);
    assert_eq!(m.paragraphs.len(), 2);
    assert!(m.paragraphs[1].wrapped && m.paragraphs[1].reason == Some("no_sentence_end"));
    assert_eq!((m.diag.second_pass_wraps_in_sentence, m.diag.first_pass_breaks), (1, 1));
    assert_eq!(m.counts().class, LayoutClass::HardWrapped, "a wrapped failing paragraph does not count toward line-form");
    // A block under MIN_PARAGRAPH_WORDS gets no in-sentence second-pass wrap.
    let short = format!("{P34}\n\nabcdefghijklmnop qrstuvwxyzabcdef\nxy z");
    assert_eq!(kinds(&short), vec![W, B, L]);
    // A break next to a list-like line is never a wrap, after a sentence end
    // too.
    let filler = "x".repeat(31);
    assert_eq!(kinds(&format!("{P34}\n\n{filler} a.\n- the next item")), vec![W, B, S]);
    assert_eq!(kinds(&format!("{P34}\n\n{filler} ab\n- the next item")), vec![W, B, L]);
    // The same breaks without the list marker are wraps.
    assert_eq!(kinds(&format!("{P34}\n\n{filler} a.\nthe next item")), vec![W, B, W]);
    assert_eq!(kinds(&format!("{P34}\n\n{filler} ab\nthe next item and more of it")), vec![W, B, W]);
}

#[test]
fn lead_changes_are_boundaries_and_never_change_w() {
    // P1-like: paragraphs indented four spaces, joined by single newlines.
    // The continuation lead is "" (after every evidence break), so the
    // breaks before the indented first lines are boundaries.
    let p = |s: &str| {
        let mut l = synth::wrap_cols(s, 50, 4);
        l[0] = l[0].clone();
        l.join("\n")
    };
    let t = format!(
        "{}\n{}",
        p("The committee met on Monday to discuss the plan for the harbour and the new bridge, and agreed the work should start."),
        p("They also asked the office to write to every family in the village about the road closures and the bus diversions.")
    );
    let m = Measure::of(&t);
    assert_eq!(m.continuation_lead, Some(""));
    let k = m.lines.iter().position(|l| t[l.start..l.body.0].len() == 4 && l.start > 0).unwrap();
    assert!(m.gaps[k - 1].lead_change && m.gaps[k - 1].kind == S);
    assert_eq!(m.paragraphs.len(), 2);
    assert_eq!(m.diag.lead_change_boundaries, 1);
    // A lead change turns first-pass evidence into a boundary, and W stays.
    let ev = "Aaaa bbbb cc dd ee ff gg hh\n  iiii jj kk ll mm nn.\nAaaa bbbb cc dd ee ff gg hh\nii jj kk ll mm nn oo pp.\nAaaa bbbb cc dd ee ff gg hh\nii jj kk ll mm nn oo pp.";
    let me = Measure::of(ev);
    assert!(me.gaps[0].evidence && me.gaps[0].lead_change && me.gaps[0].kind == L);
    assert_eq!(me.width, Some(27));
    // Ties go to the shortest lead, then the first seen.
    let tie = "Aaaa bbbb cc dd ee ff gg hh\n  ii jj kk ll mm nn oo pp.\n\nAaaa bbbb cc dd ee ff gg hh\nii jj kk ll mm nn oo pp.";
    assert_eq!(Measure::of(tie).continuation_lead, Some(""));
    let tie2 = "Aaaa bbbb cc dd ee ff gg hh\n   ii jj kk ll mm nn oo.\n\nAaaa bbbb cc dd ee ff gg hh\n  ii jj kk ll mm nn oo.";
    assert_eq!(Measure::of(tie2).continuation_lead, Some("  "));
    // Residual limit: with two continuation leads, the lines with the less
    // common one become their own paragraphs.
    let two = "Aaaa bbbb cc dd ee ff gg hh\n  ii jj kk ll mm nn oo pp.\n\nAaaa bbbb cc dd ee ff gg hh\n  ii jj kk ll mm nn oo pp.\n\nAaaa bbbb cc dd ee ff gg hh\n\tii jj kk ll mm nn oo pp.";
    let mt = Measure::of(two);
    assert_eq!(mt.continuation_lead, Some("  "));
    assert_eq!(mt.gaps.last().unwrap().kind, L);
    assert_eq!(mt.paragraphs.len(), 4);
}

#[test]
fn the_line_form_majority_counts_only_failing_paragraphs_without_a_wrap() {
    // Item 10.2(e): a punctuated paragraph and two wrapped paragraphs ending
    // unpunctuated (failing, wrapped): four of six lines fail, none counts,
    // so the text is prose.
    let t = format!("{P34}\n\noooo pppp qqqq rrrr ssss tttt uuuu\nvvvv wwww xxxx yyyy zzzz aaaa bbbb\n\ncccc dddd eeee ffff gggg hhhh iiii\njjjj kkkk llll mmmm nnnn oooo pppp");
    let c = Measure::of(&t).counts();
    assert_eq!((c.lines, c.screen_fail_lines, c.line_form_fail_lines), (6, 4, 0));
    assert_eq!(c.class, LayoutClass::HardWrapped);
    assert!(!c.passes_screen);
}

#[test]
fn a_corp_signature_stays_three_short_paragraphs_at_w_66() {
    // The order of work's counterexample: a three-line signature ending in
    // "Corp.", set off by a blank line, under a body wrapped at 66.
    let t = format!("{}\n\nJohn Smith\nSenior Manager, Gas Trading\nNorth Harbour Holdings Corp.", body(66));
    let m = Measure::of(&t);
    assert_eq!(m.width, Some(66));
    let n = m.gaps.len();
    assert_eq!(m.gaps[n - 2..].iter().map(|g| g.kind).collect::<Vec<_>>(), vec![L, L]);
    assert!(!m.gaps[n - 2].evidence && !m.gaps[n - 1].evidence);
    assert_eq!(m.paragraphs.len(), 5);
    assert_eq!(m.frames(), Frames { head: 0, tail: 3 });
}

#[test]
fn a_lowercase_greeting_above_a_lowercase_wrapped_paragraph_stays_its_own_line() {
    // The order of work's counterexample: "hi bob" with no comma, a single
    // newline, then a wrapped paragraph starting lowercase. The run from the
    // greeting fails the fit at its first break (no exemption); W comes from
    // the next paragraph; the second pass wraps the rest of the run.
    let para = wrap("the report is ready and i will send it over to you this afternoon once the figures for the third quarter are in.", 60, 0).join("\n");
    let t = format!("hi bob\n{para}\n\n{}", body(60));
    let m = Measure::of(&t);
    assert!(m.hard_wrapped);
    assert_eq!(m.gaps[0].kind, L);
    assert!(m.paragraphs[1].wrapped);
    assert_eq!(m.paragraphs[0].lines, 0..1);
    assert_eq!(m.frames().head, 1);
    assert!(m.diag.first_break_fit_fail_lowercase >= 1);
}

#[test]
fn stated_residuals_of_item_10_2() {
    // A sentence-end break before a short sign-off after a long last line
    // reads as a wrap: the sign-off joins the last paragraph.
    let b = body(60);
    let w = Measure::of(&b).width.unwrap();
    let mut last = String::from("and so");
    while last.chars().count() + 3 <= w - 2 {
        last.push_str(" ab");
    }
    last.push('.');
    assert!(last.chars().count() <= w && last.chars().count() + 4 > w);
    let t = format!("{b}\n\n{last}\nSam");
    let m = Measure::of(&t);
    assert_eq!(m.gaps.last().unwrap().kind, W);
    assert_eq!(m.frames().tail, 0);
    assert_eq!(m.diag.sign_off_proxy, 1);
    // A two-line block of 8 or more words whose wider first line precedes a
    // line ending in an abbreviation gives first-pass evidence.
    let abbr = "Please send the signed forms back to Smith\nand Jones Corp.";
    assert!(Measure::of(abbr).gaps[0].evidence);
    // Lines of an unmarked list ending within one word of W read as wraps.
    let lst = format!("{b}\n\n{}\n{}\n{}", "z".repeat(w - 2) + " a", "bb cc dd ee", "ff gg hh ii");
    let kl = kinds(&lst);
    assert_eq!(kl[kl.len() - 2..], [W, L]);
    // A trimmed first-line indent makes the first break fail the fit, so
    // its run gives no evidence: alone, the text has no W; with W from
    // another paragraph, the first break is a line break when the next word
    // fits within W.
    let trimmed = "The committee met on Monday to discuss the\nplan for the harbour and the new bridge, and agreed that\nthe work should start in the spring of next year.";
    assert_eq!((kinds(trimmed), Measure::of(trimmed).width), (vec![L, L], None));
    let with_w = format!("{trimmed}\n\n{b}");
    assert!(Measure::of(&with_w).hard_wrapped);
    assert_eq!(kinds(&with_w)[0], L);
}

#[test]
fn first_pass_rejections_are_counted_by_cause() {
    // One candidate run per cause, and one that gives evidence.
    let t = [
        "Aaaa bbbb\ncc dd ee ff gg hh ii jj kk ll.",                                        // fit fails at the first break
        "- aa bb cc dd ee ff gg\nhh ii jj kk ll mm.",                                       // a list line
        "Aaaa bbbb cc dd ee ff gg hh ii\njj kk",                                            // unpunctuated last line
        "short run\nhere.",                                                                 // block under 8 words
        "Aaaa bbbb cc dd ee ff gg hh ii jj kk ll\nmm nn oo pp qq rr ss tt uu vv ww xx\ny z.", // fit fails at a later break
        "Aaaa bbbb cc dd ee ff gg hh\nii jj kk ll mm nn oo pp.",                            // evidence
    ]
    .join("\n\n");
    let d = Measure::of(&t).diag;
    assert_eq!(d.candidate_runs, 6);
    assert_eq!((d.rejected_fit_first, d.rejected_fit_later, d.rejected_list_like, d.rejected_unpunctuated, d.rejected_short_block), (1, 1, 1, 1, 1));
    assert_eq!((d.first_pass_runs, d.first_pass_breaks, d.first_break_fit_fail_lowercase), (1, 1, 1));
    assert_eq!(d.first_pass_block_widths, 1);
}

// ------------------------------------------------------------------ 10.3

#[test]
fn child_paragraphs_take_the_wrap_flag_at_their_midpoint() {
    // P6 (amendment 7): a one-line paragraph inside a hard-wrapped parent is
    // still wrapped at the parent's W.
    let email = include_str!("fixtures/email_wrapped.txt");
    let parent = format!("{email}\nSee you all at the meeting on Friday morning in the main room.\n");
    let s = parent.find("See you").unwrap();
    let e = parent.find("main room.").unwrap() + "main room.".len();
    let src = SourceLayout::span(&parent, s, e).unwrap();
    assert_eq!(src.body_wrap, vec![(13, true)]);
    // A span over an unwrapped paragraph is not wrapped.
    let a = wrap("The committee met on Monday to discuss the plan for the harbour and the new bridge, and agreed the work should start soon.", 40, 0).join("\n");
    let long = "They also asked the office to write to every family in the village about the road closures and the bus diversions.";
    let t = format!("{a}\n\n{long}\n{long}");
    let s = t.rfind("They").unwrap();
    let sp = SourceLayout::span(&t, s, t.len()).unwrap();
    assert_eq!(sp.body_wrap.iter().map(|x| x.1).collect::<Vec<_>>(), vec![false]);
    let c = transfer("The office will write to all the families about the closures and the new bus routes this week.", &none(), &sp);
    assert_eq!(c.text.lines().count(), 1);
    // An insertion takes its paragraph's flag.
    let ins = SourceLayout::insertion(&t, t.find("soon.").unwrap() + 5, " ").unwrap();
    assert_eq!(ins.body_wrap.iter().map(|x| x.1).collect::<Vec<_>>(), vec![true]);
    // Frame lines and kept list lines are never wrapped (below).
}

// ------------------------------------------------------------------ 10.4

#[test]
fn source_and_child_edge_frames() {
    let b = body(60);
    let t = format!("Dear all,\nHello again\n\n{b}\n\nRegards,\n\nSam\n");
    let m = Measure::of(&t);
    assert_eq!(m.frames(), Frames { head: 2, tail: 2 });
    let src = SourceLayout::whole(&t);
    assert!(src.frames.head && src.frames.tail);
    assert_eq!(src.frames.head_seps, vec!["\n", "\n\n"]);
    assert_eq!(src.frames.tail_seps, vec!["\n\n", "\n\n"]);
    assert_eq!(src.separators, vec!["\n\n"]);
    // Structure fractions come from the body only: two body paragraphs.
    assert_eq!(src.structure.fractions.len(), 1);
    // A list-like edge line is no frame; a text that would have no body has
    // no frame.
    assert_eq!(Measure::of(&format!("- note\n\n{b}")).frames(), Frames { head: 0, tail: 0 });
    assert_eq!(Measure::of("Hi\n\nThanks\nSam").frames(), Frames::default());
    // The child: its outermost short non-list paragraphs on the source's
    // frame sides. Amendment 13 item 13.8: the boundary next to the body
    // takes the source's body-adjacent one on that side (the last head-side
    // one, "\n\n"; the first tail-side one), and frame-internal boundaries
    // the source's frame-internal ones from the frame's outer edge (amendment
    // 10 gave the greeting the source's first head boundary, "\n").
    let child = "Hi there,\n\nThe council met on Tuesday evening to discuss the plan for the town centre and agreed it.\n\nThe vote was postponed until next month when the full report will be ready for everyone.\n\nCheers,\nAnn";
    let c = transfer(child, &none(), &src);
    assert_eq!((c.record.head_frame_lines, c.record.tail_frame_lines), (1, 2));
    assert!(c.text.starts_with("Hi there,\n\nThe council"), "{:?}", c.text);
    assert!(c.text.ends_with("\n\nCheers,\n\nAnn\n"), "{:?}", c.text);
    assert_eq!(c.frame_lines.len(), 3);
    // A source sign-off "Thanks,\nSam" after a blank line and a child
    // sign-off on one line: the body-to-tail boundary stays the blank line
    // (amendment 10 gave it "\n", the source's frame-internal boundary).
    let signed = SourceLayout::whole(&format!("Hi Bob,\n\n{b}\n\nThanks,\nSam\n"));
    let one = transfer("Hello Ann,\n\nThe council met on Tuesday evening to discuss the plan for the town centre and agreed it.\n\nBest, Ann", &none(), &signed);
    assert!(one.text.ends_with("agreed it.\n\nBest, Ann\n"), "{:?}", one.text);
    let three = transfer("Hello Ann,\n\nThe council met on Tuesday evening to discuss the plan for the town centre and agreed it.\n\nBest,\nAnn\nNorth Office", &none(), &signed);
    assert!(three.text.ends_with("agreed it.\n\nBest,\nAnn\nNorth Office\n"), "{:?}", three.text);
    // With no frame-internal boundary on a side, the body-adjacent one is
    // reused (counted).
    let two_head = transfer("Hello Ann,\nGood morning\n\nThe council met on Tuesday evening to discuss the plan for the town centre and agreed it.\n\nBest,\nAnn", &none(), &signed);
    assert!(two_head.text.starts_with("Hello Ann,\n\nGood morning\n\nThe council"), "{:?}", two_head.text);
    assert_eq!(two_head.record.frame_internal_reused, 1);
    // No frame on a side where the source has none.
    let tail_only = SourceLayout::whole(&format!("{b}\n\nThanks,\nSam"));
    let c2 = transfer(child, &none(), &tail_only);
    assert_eq!((c2.record.head_frame_lines, c2.record.tail_frame_lines), (0, 2));
    // A child whose every paragraph is short has no frame (amendment 13 item
    // 13.8 reads item 10.4 as the source rule; amendment 10's code kept the
    // last one as body and took the rest as frames, head side first).
    let all_short = transfer("Hi there,\n\nShort line\n\nBye", &none(), &src);
    assert_eq!((all_short.record.head_frame_lines, all_short.record.tail_frame_lines), (0, 0));
    assert!(all_short.body.is_some() && all_short.frame_lines.is_empty());
    // Mixed children, items and insertions have no frames.
    let s = t.find("The committee").unwrap();
    assert!(!SourceLayout::span(&t, s, s + 13).unwrap().frames.head);
    assert!(SourceLayout::insertion(&t, s, " ").unwrap().frames == Default::default());
}

#[test]
fn frame_lines_take_the_source_frame_whitespace_and_are_never_wrapped() {
    let b = body(40);
    let t = format!("  Hi Bob,  \n\n{b}\n\n  Thanks, \n Sam\n");
    let src = SourceLayout::whole(&t);
    assert_eq!(src.frames.head_ws, vec![("  ".to_string(), "  ".to_string())]);
    assert_eq!(src.frames.tail_ws, vec![("  ".to_string(), " ".to_string()), (" ".to_string(), String::new())]);
    let child = "Hello there everybody in the long line,\n\nThe council met on Tuesday evening to discuss the plan for the town centre and agreed it.\n\nBest regards to all of you,\nAnn";
    let c = transfer(child, &none(), &src);
    let lines: Vec<&str> = c.text.lines().collect();
    assert_eq!(lines[0], "  Hello there everybody in the long line,  ");
    assert_eq!(lines[lines.len() - 2], "  Best regards to all of you, ");
    assert_eq!(lines[lines.len() - 1], " Ann");
}

// ------------------------------------------------------------------ 10.5

#[test]
fn classify_all_agrees_with_classify() {
    let cases = [
        "The ferry left the harbour at dawn and the gulls followed it out.",
        "Too short.",
        "No end to this paragraph of more than eight words here",
        "- a list item that is long enough to pass the length test.",
        "> a quoted line that is long enough to pass the length test.",
        "See http://example.org for the details of the plan for the town.",
        "The [[link]] markup was left in this paragraph of the old wiki page.",
        "Write to me at someone@example.org about the plan for the harbour.",
        "Click here to read more about the plan for the harbour and bridge.",
        "Les \u{e9}l\u{e8}ves \u{e9}taient tr\u{e8}s \u{e9}nerv\u{e9}s \u{e0} l'\u{e9}cole aujourd'hui \u{e7}a.",
        "He said \"this is a very long quotation that goes on for many words\" today.",
        "3. A numbered item that is long enough to pass the length test too.",
        "A <b>tag</b> was left in this paragraph of the page about the town.",
        "",
    ];
    for p in cases {
        let all = classify_all(p);
        let c = classify(p, false).reason;
        assert_eq!(all.is_empty(), c.is_none(), "{p:?}: {all:?} vs {c:?}");
        if let Some(r) = c {
            assert_eq!(v1_reason(all[0]), r, "{p:?}");
        }
    }
    // The list test runs on every line and is split in two.
    assert_eq!(classify_all("Line one of the text that passes the screen.\n> quoted")[0], QUOTED_LINE);
    assert_eq!(classify_all("> quoted text\n- a marker line here.")[..2], [QUOTED_LINE, LIST_MARKER]);
    assert!(list_like("> x") && v1_reason(LIST_MARKER) == "list_like_line");
}

#[test]
fn the_licence_screen_licenses_body_and_frames_separately() {
    let b = body(66);
    let parent = format!("Hi Bob,\n\n{b}\n\nThanks,\nSam\n");
    let src = SourceLayout::whole(&parent);
    assert!(src.licence.is_empty());
    assert_eq!(src.frame_licence, vec!["no_sentence_end", "short_paragraph"]);
    // A body paragraph cut off mid-sentence is rejected: the greeting's
    // licence never licenses the body.
    let cut = "Hello Bob,\n\nThe council met on Tuesday evening to discuss the plan for the town centre and agreed it.\n\nThe vote was postponed until next month when the full report will be ready for\n\nBest,\nAnn";
    let c = child_v2(&parent, None, &none(), cut);
    assert!(reasons_of(&c).contains(&"form_line_no_sentence_end".to_string()), "{:?}", c.reasons);
    assert!(c.record["unlicensed_reasons"].as_array().unwrap().iter().any(|x| x == "line:no_sentence_end"));
    // A child whose every paragraph is short has no frame (item 13.8): its
    // body is rebuilt into one short paragraph that the body licence
    // rejects (short_paragraph is a frame reason only), and the greeting line
    // fails the line test.
    let short = child_v2(&parent, None, &none(), "Hello Bob,\n\nThe vote was postponed.");
    assert_eq!(short.record["transfer"]["head_frame_lines"], 0);
    assert!(short.reasons.contains(&"form_paragraph_short_paragraph".to_string()), "{:?}", short.reasons);
    // The same child ending its body properly passes, frames and all.
    let ok = cut.replace("ready for\n", "ready for everyone.\n");
    let c = child_v2(&parent, None, &none(), &ok);
    assert!(c.reasons.is_empty(), "{:?}", c.reasons);
    assert_eq!(c.record["form_screen"], FORM_LICENCE_SCREEN_ID);
    assert_eq!(c.record["form_licence"]["frame"], serde_json::json!(["no_sentence_end", "short_paragraph"]));
    // A frame line with a reason outside the frame licence is rejected.
    let bad_frame = ok.replace("Hello Bob,", "Mail bob@example.org");
    let c = child_v2(&parent, None, &none(), &bad_frame);
    assert!(c.reasons.contains(&"form_frame_boilerplate_term".to_string()), "{:?}", c.reasons);
}

#[test]
fn a_licensed_reason_is_accepted_and_an_unlicensed_one_rejected() {
    // A source whose body holds a list: list_marker is licensed, quoted_line
    // is not.
    let parent = "The club met on Monday and agreed the plan for the spring walks this year.\n\n- a waterproof coat for the rain\n- sturdy boots for the hills\n\nEveryone should bring the items above to the first walk of the season.";
    let src = SourceLayout::whole(parent);
    assert!(!src.line_form);
    assert!(src.licence.iter().any(|r| r == LIST_MARKER) && !src.licence.iter().any(|r| r == QUOTED_LINE));
    let child = "The group met on Tuesday and settled the plan for the summer walks.\n\n- a warm hat for the wind\n- a flask of tea\n\nPlease bring all of these things to the first walk of the year.";
    let c = child_v2(parent, None, &none(), child);
    assert!(c.reasons.is_empty(), "{:?}", c.reasons);
    let quoted = child.replace("- a warm", "> a warm");
    let q = child_v2(parent, None, &none(), &quoted);
    assert!(q.reasons.contains(&"form_line_list_like".to_string()), "{:?}", q.reasons);
    assert!(q.record["unlicensed_reasons"].as_array().unwrap().iter().any(|x| x == "line:quoted_line"));
}

#[test]
fn line_form_children_stay_unscreened_with_would_be_reasons_counted() {
    let irc = include_str!("fixtures/irc.txt");
    let c = child_v2(irc, None, &none(), "- a list line\nanother line without an end");
    assert_eq!(c.record["form_screen_applied"], false);
    assert!(c.reasons.is_empty(), "{:?}", c.reasons);
    assert!(!c.record["would_be_unlicensed"].as_array().unwrap().is_empty());
}

// ------------------------------------------------------------------ 10.6

#[test]
fn without_a_kept_unit_the_body_is_v1s_rebuild() {
    let b = body(66);
    let parent = format!("Hi Bob,\n\n{b}\n\nThanks,\nSam\n");
    let src = SourceLayout::whole(&parent);
    let child = "Hello Bob,\n\nThe council met on Tuesday evening to discuss the plan.  It agreed the plan in the end.\nThe vote was postponed until next month when the report will be ready.\n\nBest,\nAnn";
    let t = transfer(child, &none(), &src);
    let body_raw = "The council met on Tuesday evening to discuss the plan.  It agreed the plan in the end.\nThe vote was postponed until next month when the report will be ready.";
    let r = rebuild_child(body_raw, &none(), &src.structure, false);
    let (a, z) = t.body.unwrap();
    assert_eq!(text::squash(&t.text[a..z]), r.paragraphs.join(" "));
    assert_eq!((t.record.kept_list_lines, t.record.closed_paragraphs), (0, 0));
}

#[test]
fn kept_list_units_keep_their_lines_as_fixed_boundaries() {
    let parent = "The club met on Monday and agreed the plan for the spring walks this year.\n\n- a waterproof coat for the rain\n- sturdy boots for the hills\n\nEveryone should bring the items above to the first walk of the season.";
    let src = SourceLayout::whole(parent);
    let child = "The group met on Tuesday and settled the plan for the summer walks. It will start in June. People should come early.\n- a warm hat for the wind\n- a flask of tea\nPlease bring all of these things to the first walk of the year.";
    let t = transfer(child, &none(), &src);
    assert_eq!(t.record.kept_list_lines, 2);
    let lines: Vec<&str> = t.text.lines().filter(|l| !l.trim().is_empty()).collect();
    assert!(lines.contains(&"- a warm hat for the wind") && lines.contains(&"- a flask of tea"), "{lines:?}");
    // Never merged into prose.
    assert!(!t.text.contains(". - a"), "{:?}", t.text);
    // The source's four paragraphs give three targets: one falls in the
    // prose before the list and is placed; two fall on or inside the kept
    // unit and are realised by its fixed boundaries.
    assert_eq!(t.layout.target_breaks, 3);
    assert_eq!((t.layout.realised_breaks, t.record.targets_on_fixed_boundaries, t.layout.dropped_breaks), (1, 2, 0));
}

#[test]
fn an_unpunctuated_body_paragraph_closes_when_licensed() {
    let p = |s: &str| wrap(s, 48, 0).join("\n");
    let parent = format!(
        "{}\n\n{}",
        p("The committee met on Monday to discuss the plan for the harbour and agreed the work should start soon."),
        p("they also asked the office to write to every family in the village about the closures and the diversions")
    );
    let src = SourceLayout::whole(&parent);
    assert!(src.licence.iter().any(|r| r == "no_sentence_end"));
    let child = "the council wrote to every family in the town about the bus changes and the parking\n\nThe vote was postponed until next month when the full report will be ready for everyone to read.";
    let t = transfer(child, &none(), &src);
    assert_eq!(t.record.closed_paragraphs, 1);
    let paras: Vec<&str> = t.text.split("\n\n").collect();
    assert_eq!(paras.len(), 2, "{:?}", t.text);
    assert!(paras[0].lines().count() > 1, "wrapped like other body paragraphs");
    assert!(!text::ends_sentence(&text::squash(paras[0])));
    let c = child_v2(&parent, None, &none(), child);
    assert!(c.reasons.is_empty(), "{:?}", c.reasons);
}

#[test]
fn whitespace_by_role() {
    // Paragraph-first lines indented four spaces, continuation lines not,
    // trailing spaces on wrapped lines only.
    let p1 = synth::wrap_cols("The committee met on Monday to discuss the plan for the harbour and the new bridge, and agreed the work should start.", 50, 4);
    let p2 = synth::wrap_cols("They also asked the office to write to every family in the village about the road closures and the bus diversions.", 50, 4);
    let lay = |ls: &[String]| ls.iter().enumerate().map(|(k, l)| if k + 1 < ls.len() { format!("{l} ") } else { l.clone() }).collect::<Vec<_>>().join("\n");
    let src_text = format!("{}\n{}", lay(&p1), lay(&p2));
    let src = SourceLayout::whole(&src_text);
    assert_eq!(src.pools.first_leads, vec!["    ", "    "]);
    assert!(src.pools.cont_leads.iter().all(|l| l.is_empty()));
    assert!(src.pools.wrap_trails.iter().all(|t| t == " "));
    assert!(src.pools.final_trails.iter().all(|t| t.is_empty()));
    let child = "The council members gathered on Tuesday evening to review the proposal for the waterfront. They wrote to all of the families in the town about the closures and the new bus routes.";
    let t = transfer(child, &none(), &src);
    let m = Measure::of(&t.text);
    for p in &m.paragraphs {
        for i in p.lines.clone() {
            let first = i == p.lines.start;
            assert_eq!(m.lead(i), if first { "    " } else { "" }, "{:?}", t.text);
            let last = i + 1 == p.lines.end;
            assert_eq!(m.trail(i), if last { "" } else { " " }, "{:?}", t.text);
            assert!(m.columns(i) <= src.width.unwrap());
        }
    }
    assert_eq!(t.record.ws_pool_fallbacks, 0);
    // An empty pool gives "" and is counted: a span with no continuation
    // line under a wrapped parent.
    let s = src_text.find("They").unwrap();
    let e = src_text.find("road").unwrap() - 1;
    let mut sp = SourceLayout::span(&src_text, s, e).unwrap();
    sp.pools.cont_leads.clear();
    let c = transfer(child, &none(), &sp);
    assert!(c.record.ws_pool_fallbacks > 0);
}

// ------------------------------------------------------------------ 10.7, 10.8

#[test]
fn ids_and_the_closed_population_list() {
    assert_eq!((MEASURE_ID, LAYOUT_TRANSFER_ID, FORM_LICENCE_SCREEN_ID), ("layout_measure_v2", "layout_transfer_v2", "cc_form_licence_screen_v1"));
    let rec = Profile::CohortV2.record();
    assert_eq!((rec["profile"].as_str(), rec["layout_measure"].as_str(), rec["layout_transfer"].as_str()), (Some(PROFILE_V2), Some(MEASURE_ID), Some(LAYOUT_TRANSFER_ID)));
    assert_eq!(rec["form_screen"]["id"], FORM_LICENCE_SCREEN_ID);
    let c = child_v2("A short parent line.", None, &none(), "A child line that is long enough to stand here.");
    assert_eq!((c.record["layout_measure"].as_str(), c.record["layout_transfer"].as_str()), (Some(MEASURE_ID), Some(LAYOUT_TRANSFER_ID)));
    assert_eq!(c.record["source"]["id"], MEASURE_ID);
    assert_eq!(R2_POPULATIONS.len(), 14);
    for p in ["train2", "deviter2_t", "deviter2_c", "deviter2_mock", "devselect2_t", "devselect2_c", "cal2_t", "cal2_c", "cal2_mock", "test2_t", "test2_c", "test2_mock", "span_select2", "span_test2"] {
        assert!(is_r2_population(p), "{p}");
    }
    for p in ["cal2", "train", "Train2", " train2", "test2", ""] {
        assert!(!is_r2_population(p), "{p:?}");
    }
    // V21's seed form is unchanged.
    assert_eq!(norm_prior_seed("train2"), format!("{:x}", <sha2::Sha256 as sha2::Digest>::digest(b"slop_ninja_a_comp_r2_plan_v1:norm_prior:train2")));
}

// ------------------------------------------------------------------ design B's adversarial cases

#[derive(Debug, PartialEq, Eq)]
struct Tally {
    wrap: usize,
    line_break: usize,
    hard_wrapped: usize,
    line_form: usize,
    paragraphs: usize,
    unwrapped_lines: usize,
}

fn tally(docs: &[String]) -> Tally {
    let mut t = Tally { wrap: 0, line_break: 0, hard_wrapped: 0, line_form: 0, paragraphs: 0, unwrapped_lines: 0 };
    for d in docs {
        let m = Measure::of(d);
        for g in m.gaps.iter().filter(|g| g.in_sentence()) {
            t.wrap += (g.kind == W) as usize;
            t.line_break += (g.kind == L) as usize;
        }
        let c = m.counts();
        t.hard_wrapped += c.hard_wrapped as usize;
        t.line_form += (c.class == LayoutClass::LineForm) as usize;
        t.paragraphs += c.paragraphs;
        t.unwrapped_lines += c.diag.unwrapped_lines;
    }
    t
}

#[test]
fn design_b_adversarial_generators() {
    // Readings under amendment 10 (design A with its grafts), pinned. The
    // design-B prototype's log gives amendment 7's readings beside them.
    let pins = [
        // A1/A1b: a counted indent trimmed at admission (residual: the first
        // break is a line break when the next word fits within W).
        // Amendment 13 (items 13.2-13.5) changed every pin with unwrapped
        // lines or line breaks: W now counts the evidence runs' last lines
        // and a line within one word of W is not unwrapped. Amendment 10's
        // pins are in the comments.
        // (amendment 10: wrap 1453, line_break 192, paragraphs 508, unwrapped 41)
        ("A1", Tally { wrap: 1513, line_break: 132, hard_wrapped: 60, line_form: 0, paragraphs: 447, unwrapped_lines: 0 }),
        // (amendment 10: 1388, 218, 59, 1, 536, 45)
        ("A1b", Tally { wrap: 1452, line_break: 154, hard_wrapped: 59, line_form: 1, paragraphs: 468, unwrapped_lines: 0 }),
        // A2: an indent added after wrapping makes the first line the
        // block's widest, so the fit test fails for many runs (open item).
        // (amendment 10: 1398, 223, 60, 0, 556, 108)
        ("A2", Tally { wrap: 1505, line_break: 116, hard_wrapped: 60, line_form: 0, paragraphs: 433, unwrapped_lines: 2 }),
        // A2b: a tab the wrapper expanded to 8 counts one column (residual).
        // (amendment 10: 1005, 529, 53, 11, 850, 111)
        ("A2b", Tally { wrap: 1164, line_break: 370, hard_wrapped: 53, line_form: 7, paragraphs: 684, unwrapped_lines: 2 }),
        // A3/A3b: single-newline paragraphs ending unpunctuated: no W in 38
        // and 41 documents (residual).
        // (amendment 10: 299, 1182, 22, 45, 1309, 143; 280, 1170, 19, 45, 1275, 101)
        ("A3", Tally { wrap: 467, line_break: 1014, hard_wrapped: 22, line_form: 38, paragraphs: 1117, unwrapped_lines: 0 }),
        ("A3b", Tally { wrap: 411, line_break: 1039, hard_wrapped: 19, line_form: 41, paragraphs: 1130, unwrapped_lines: 0 }),
        // A4: the title stays its own paragraph in all 60 documents.
        // (amendment 10: 938, 79, 60, 0, 259, 12)
        ("A4", Tally { wrap: 957, line_break: 60, hard_wrapped: 60, line_form: 0, paragraphs: 240, unwrapped_lines: 0 }),
        // A5/A5b: commas at line ends do not matter (no verse rule).
        ("A5", Tally { wrap: 1652, line_break: 0, hard_wrapped: 60, line_form: 0, paragraphs: 300, unwrapped_lines: 0 }),
        ("A5b", Tally { wrap: 927, line_break: 0, hard_wrapped: 60, line_form: 0, paragraphs: 480, unwrapped_lines: 0 }),
        // A6: one W per document: the block wrapped at 40 splits at its
        // sentence-end breaks (residual).
        // (amendment 10: paragraphs 98, unwrapped 1)
        ("A6", Tally { wrap: 542, line_break: 0, hard_wrapped: 40, line_form: 0, paragraphs: 97, unwrapped_lines: 0 }),
        // A7: the short one-line block and the long unwrapped block stay
        // their own paragraphs.
        ("A7", Tally { wrap: 210, line_break: 0, hard_wrapped: 40, line_form: 0, paragraphs: 120, unwrapped_lines: 40 }),
        // (amendment 10: 316, 1152, 18, 45, 1263, 61)
        ("A8", Tally { wrap: 387, line_break: 1081, hard_wrapped: 18, line_form: 42, paragraphs: 1184, unwrapped_lines: 2 }),
        // A8b: indented single-newline paragraphs read exactly (lead changes).
        ("A8b", Tally { wrap: 1286, line_break: 0, hard_wrapped: 60, line_form: 0, paragraphs: 240, unwrapped_lines: 0 }),
    ];
    for (name, want) in pins {
        assert_eq!(tally(&synth::adversarial(name)), want, "{name}");
    }
    let titles = synth::adversarial("A4").iter().filter(|d| Measure::of(d).gaps[0].kind == L).count();
    assert_eq!(titles, 60);
}

#[test]
fn design_b_hand_cases() {
    use LayoutClass::*;
    let want: &[(&str, LayoutClass, Option<usize>, &[GapKind])] = &[
        // Changed by amendment 10 (item 10.1 with 10.2(d)): with columns the
        // indented short lines of the first stanza pass the fit test, so it
        // gives first-pass evidence (W 33); the unindented lines are lead
        // changes and the second stanza's lines are unwrapped. Amendment 7
        // read LineForm, no W, every break a line break (open item).
        // Amendment 13 item 13.5: the second stanza's lines are 1-2 columns
        // wider than W, within one word, so they are no longer unwrapped and
        // the stanza reads like the first (amendment 10: three line breaks).
        ("verse_uneven", HardWrapped, Some(33), &[W, L, W, B, W, L, W]),
        // W counts the evidence runs' last lines (item 13.5): 37, was 36.
        ("verse_even", HardWrapped, Some(37), &[W, W, B, W, W]),
        ("verse_modern_unpunct", LineForm, None, &[L, L, L]),
        ("titled", Prose, None, &[L, S]),
        ("lead_in", Prose, None, &[L, S]),
        ("signed", LineForm, None, &[B, L, L]),
        ("signoff_long", LineForm, None, &[B, L]),
        ("unmarked_list", LineForm, None, &[L, L]),
        ("address", LineForm, None, &[L, L]),
        ("hand_break", LineForm, None, &[L]),
        ("prose_then_list", LineForm, None, &[S, S, L, L, L]),
        ("raid_short_last", Prose, None, &[S, S, S, S]),
        ("email_greeting", HardWrapped, Some(67), &[B, W, W, W, B, W, W, W, B, L]),
        ("hyphen_wrap", LineForm, None, &[L, L]),
        ("unpunct_no_periods", LineForm, None, &[L, L]),
        ("lower_greeting_inherit", HardWrapped, Some(67), &[L, B, W, W]),
        // W in columns: 51 (amendment 7: 49 in body characters).
        ("indented_wrapped", HardWrapped, Some(51), &[W, W, W]),
    ];
    for &(name, class, width, ks) in want {
        let t = synth::CASES.iter().find(|c| c.0 == name).unwrap().1;
        let c = Measure::of(t).counts();
        assert_eq!((c.class, c.width, kinds(t).as_slice()), (class, width, ks), "{name}");
    }
}

// ------------------------------------------------------------------ stop invariants (item 10.12)

/// Synthetic sources: the probes, the adversarial generators and the hand
/// cases.
fn sources() -> Vec<String> {
    let mut v: Vec<String> = Vec::new();
    v.extend(synth::p1(4, "\n").into_iter().take(10));
    v.extend(synth::p4().into_iter().take(15));
    v.extend(synth::p4b().into_iter().take(10));
    v.extend(synth::p5().into_iter().take(10).map(|x| x.0));
    v.extend(synth::p2().into_iter().take(10).map(|x| x.0));
    for n in ["A1", "A2", "A2b", "A3", "A4", "A6", "A7", "A8"] {
        v.extend(synth::adversarial(n).into_iter().take(8));
    }
    v.extend(synth::CASES.iter().map(|c| c.1.to_string()));
    v
}

#[test]
fn no_wrap_lies_next_to_an_unwrapped_or_list_like_line() {
    // Item 10.12's first stop invariant as amendment 13 item 13.10 states
    // it: no wrap next to an unwrapped line; no second-pass wrap next to a
    // list_like line; a first-pass wrap next to a list_like line only when
    // that line is a continuation (not its run's first line, no list_like
    // line next to it in the run).
    let mut srcs = sources();
    srcs.extend(dash_sources());
    let mut continuations = 0;
    for s in srcs {
        let m = Measure::of(&s);
        let list = |i: usize| list_like(m.body(i));
        let in_run = |i: usize| i > 0 && m.gaps[i - 1].in_sentence();
        for (i, g) in m.gaps.iter().enumerate() {
            if g.kind == W {
                assert!(!m.unwrapped[i] && !m.unwrapped[i + 1], "unwrapped neighbour");
                if !g.evidence {
                    assert!(!list(i) && !list(i + 1), "list-like neighbour of a second-pass wrap");
                } else {
                    for k in [i, i + 1].into_iter().filter(|&k| list(k)) {
                        assert!(in_run(k), "a run's list-like first line");
                        assert!(!(in_run(k) && list(k - 1)), "consecutive list-like lines");
                        assert!(!(k + 1 < m.lines.len() && m.gaps[k].in_sentence() && list(k + 1)), "consecutive list-like lines");
                        continuations += 1;
                    }
                }
            }
        }
    }
    assert!(continuations > 0);
}

/// Prose wrapped at 64 whose sentences hold " -- ", an em dash or "3.5", so
/// a wrap can put one at a line start (the review's F12).
fn dash_sources() -> Vec<String> {
    let mut r = synth::Lcg(7);
    let mut out = Vec::new();
    for token in ["--", "\u{2014}", "3.5"] {
        for _ in 0..20 {
            let ps: Vec<String> = (0..3)
                .map(|_| {
                    let sents: Vec<String> = (0..4)
                        .map(|_| {
                            let mut w: Vec<String> = synth::sentence(&mut r, true).split(' ').map(str::to_string).collect();
                            let k = 1 + r.below(w.len() as u64 - 1) as usize;
                            w.insert(k, token.to_string());
                            w.join(" ")
                        })
                        .collect();
                    wrap(&sents.join(" "), 64, 0).join("\n")
                })
                .collect();
            out.push(ps.join("\n\n"));
        }
    }
    out
}

#[test]
fn accepted_children_keep_budgets_frames_and_kept_units() {
    let children = [
        "Hello there,\n\nThe council met on Tuesday evening to discuss the new parking plan for the town centre. Members argued about the cost of the scheme for more than an hour and nobody gave way.\n\nSome residents said the plan would finally fix the traffic on the high street. Others were not convinced.\n\nBest,\nAnn",
        "The council met on Tuesday evening to discuss the new parking plan for the town centre. Members argued about the cost of the scheme for more than an hour and nobody gave way. Some residents said the plan would finally fix the traffic on the high street. Others were not convinced at all.",
        "The group met on Tuesday and settled the plan for the summer walks this year.\n- a warm hat for the wind\n- a flask of tea for the hills\nPlease bring all of these things to the first walk of the year.",
    ];
    let mut checked = 0;
    for s in sources() {
        let src = SourceLayout::whole(&s);
        if src.line_form {
            continue;
        }
        for raw in children {
            let c = child_v2(&s, None, &none(), raw);
            if !c.reasons.is_empty() {
                continue;
            }
            checked += 1;
            let t = transfer(raw, &none(), &src);
            assert_eq!(t.text, c.text);
            // No child frame on a side where the source has none.
            assert!(src.frames.head || t.record.head_frame_lines == 0);
            assert!(src.frames.tail || t.record.tail_frame_lines == 0);
            // No wrapped line over its budget that holds more than one word.
            if let Some(w) = src.width {
                let m = Measure::of(&t.text);
                for &(a, b) in &t.paragraphs {
                    let ls: Vec<usize> = (0..m.lines.len()).filter(|&i| m.lines[i].body.0 >= a && m.lines[i].body.1 <= b).collect();
                    if ls.len() > 1 {
                        for i in ls {
                            assert!(m.columns(i) <= w || !m.body(i).contains(' '), "over budget");
                        }
                    }
                }
            }
            // No kept unit merged into prose; each kept list line is a unit.
            if t.record.kept_list_lines > 0 {
                let units: Vec<&str> = t.paragraphs.iter().map(|&(a, b)| &t.text[a..b]).collect();
                assert!(units.contains(&"- a warm hat for the wind") && units.contains(&"- a flask of tea for the hills"));
            }
            assert!(!t.text.contains(". - a"), "kept unit merged");
            // No body paragraph under MIN_PARAGRAPH_WORDS made by a placed break.
            if t.record.kept_list_lines == 0 && t.record.closed_paragraphs == 0 && t.layout.realised_breaks > 0 {
                for &(a, b) in t.paragraphs.iter().filter(|r| !t.frame_lines.contains(r)) {
                    assert!(text::words(&t.text[a..b]) >= MIN_PARAGRAPH_WORDS, "short paragraph from a placed break");
                }
            }
        }
    }
    assert!(checked > 100, "{checked}");
}

#[test]
fn excerpt_layout_sources_transfer_onto_themselves() {
    // Self-identity: an excerpt-layout source whose paragraphs all pass,
    // transferred onto itself, gives the same skeleton and squashed lines.
    let srcs = [
        "The ferry left the harbour at dawn and the gulls followed it out to sea.\n\nBy noon the wind had dropped and the passengers came up on deck to eat.\n\nThe crossing took four hours and nobody was sick for once this year.",
        "The museum reopened last week after two years of repairs to the roof and the hall.",
        "Dr. Hale met Mr. Ortiz at 3 p.m. on Jan. 4 near the old border post. They spoke briefly.\n\nJ. Ortiz later wrote that the talk went well, and Prof. Lin agreed with him on most points.",
    ];
    for s in srcs {
        let c = Measure::of(s).counts();
        assert!(c.class == LayoutClass::Excerpt && c.passes_screen);
        let t = transfer(s, &none(), &SourceLayout::whole(s));
        assert_eq!(Measure::of(&t.text).counts().skeleton_sha256, c.skeleton_sha256);
        assert_eq!(t.text, s);
        assert_eq!(t.text, normalise_child(s, &none(), &Structure::whole(s)).text);
    }
}

#[test]
fn items_screen_each_edit_with_its_own_licence() {
    // An item over a parent with a list: an edit in the list region may keep
    // a list line, an edit in the prose region may not.
    let parent = "The club met on Monday and agreed the plan for the spring walks this year.\n\n- a waterproof coat for the rain\n- sturdy boots for the hills\n\nEveryone should bring the items above to the first walk of the season.";
    let ls = parent.find("- sturdy").unwrap();
    let le = ls + "- sturdy boots for the hills".len();
    let ps = parent.find("Everyone").unwrap();
    let it = item_v2(parent, &none(), &[EditV2 { span: (ls, le), insert_sep: None, raw: "- a warm hat for the wind" }, EditV2 {
        span: (ps, parent.len()),
        insert_sep: None,
        raw: "Please bring all of these things to the first walk of the year.",
    }]);
    assert!(it.reasons.is_empty(), "{:?}", it.reasons);
    let bad = item_v2(parent, &none(), &[EditV2 { span: (ps, parent.len()), insert_sep: None, raw: "- please bring a hat" }]);
    assert!(bad.reasons.contains(&"form_line_list_like".to_string()), "{:?}", bad.reasons);
    // A donor child gets frames and the licence screen too.
    let donor = SourceLayout::whole(&format!("Hi Bob,\n\n{}\n\nThanks,\nSam\n", body(66)));
    let d = donor_child_v2(&donor, &none(), "Hello Ann,\n\nThe council met on Tuesday evening to discuss the plan for the town centre and agreed it.\n\nBest,\nBo");
    assert!(d.reasons.is_empty(), "{:?}", d.reasons);
    assert_eq!(d.record["transfer"]["head_frame_lines"], 1);
    assert_eq!(transfer("x", &none(), &donor).record.path, Path::Prose);
}
