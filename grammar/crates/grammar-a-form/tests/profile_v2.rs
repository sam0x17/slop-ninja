//! `cc_cohort_profile_v2` (protocol A2 §6.2, as amended by amendments 7 and
//! 10) on synthetic fixtures: the layout measurement `layout_measure_v2`,
//! `layout_transfer_v2`, the v2 ≡ v1 byte identity on excerpt-layout
//! parents, line-form sources, the licence screen (a passing source gets
//! §6.2's screen), the echo screen on every child, the population x domain
//! prior and records without text. `tests/a10_rules.rs` and
//! `tests/a10_probes.rs` hold amendment 10's rules and probes. Every text
//! here and in `tests/fixtures/` is synthetic.
use grammar_a_form::conventions::{Choice, Conventions, Dash, Ellipsis, Prior, Quote, Range, counts, resolve};
use grammar_a_form::layout::{Structure, excerpt_layout, normalise_child, rebuild_child};
use grammar_a_form::measure::{GapKind, LayoutClass, Measure};
use grammar_a_form::profile::{
    EditV2, PROFILE_V2, Part, PriorV2, Profile, assemble_item, child_v2, donor_child_v2, every_class, every_class_t_block, item_v2, norm_prior_seed,
    resolve_v2, splice,
};
use grammar_a_form::screen::paragraph_screen;
use grammar_a_form::text;
use grammar_a_form::transfer::{LINE_FORM_FALLBACK, Path, SourceLayout, transfer, wrap};
use serde_json::Value;
use std::collections::BTreeMap;

const IRC: &str = include_str!("fixtures/irc.txt");
const VERSE: &str = include_str!("fixtures/verse.txt");
const TRANSCRIPT: &str = include_str!("fixtures/transcript.txt");
const LIST: &str = include_str!("fixtures/list.txt");
const TALK: &str = include_str!("fixtures/talk_page.txt");
const RAID: &str = include_str!("fixtures/raid_human.txt");
const EMAIL: &str = include_str!("fixtures/email_wrapped.txt");
const MIXED_SEPS: &str = include_str!("fixtures/mixed_separators.txt");
/// The hard-wrapped email with a greeting before it and a sign-off after it.
const EMAIL_GREETING: &str = include_str!("fixtures/email_greeting.txt");
/// The RAID-like human with a short closing line.
const RAID_SHORT_LAST: &str = include_str!("fixtures/raid_short_last.txt");
/// Greedy-wrapped at 47 characters with curly quotes, em dashes and accented
/// letters (character and byte widths differ); the longest line is the
/// second paragraph's second.
const WRAPPED_UNICODE: &str = include_str!("fixtures/wrapped_unicode.txt");
/// Two prose lines, then a four-item list.
const PROSE_THEN_LIST: &str = include_str!("fixtures/prose_then_list.txt");
/// Greedy-wrapped at 49 characters, every line indented by two spaces.
const INDENTED_WRAPPED: &str = include_str!("fixtures/indented_wrapped.txt");

/// A generated child in its own form: trailing spaces, single breaks, paired
/// emphasis, ASCII quotes, a spaced hyphen and an ellipsis.
const CHILD_PROSE: &str = "The council met on Tuesday evening to discuss the new parking plan for the town centre.  \nMembers argued about the cost of the scheme for more than an hour - and nobody gave way.\n\nSome residents said the plan would **finally** fix the traffic on the high street. Others were not convinced, and one shop owner called it \"a waste of money\" in front of the whole room...\n\nThe vote was postponed until next month. The full report from the engineers will be ready by then for everyone to read. A public meeting will follow in the spring.";
/// A line-form child: chat lines with a blank-line break.
const CHILD_LINES: &str = "<tern> ok the cache is cleared now  \n<maple> great, run the build again\n\n<tern> it passed this time\n<maple> thanks for the help";
const CHILD_VERSE: &str = "The swallows leave the barn at dusk\nand circle round the yard\nthe farmer shuts the heavy door\nand bolts it twice and hard\n\nThe kettle sings upon the stove\nthe dog lies by the fire\nand somewhere out beyond the hill\na bell rings in the spire";
const CHILD_LIST: &str = "Things to pack:\n- a torch for the evening\n- spare socks and a hat\n- the train tickets";

fn none() -> Conventions {
    fn c<K>() -> Choice<K> {
        Choice { value: None, source: "none" }
    }
    Conventions { double: c(), single: c(), dash: c(), range: c(), ellipsis: c() }
}

fn fixed(d: Quote, dash: Dash, range: Range, ell: Ellipsis) -> Conventions {
    fn c<K>(v: K) -> Choice<K> {
        Choice { value: Some(v), source: "parent" }
    }
    Conventions { double: c(d), single: c(d), dash: c(dash), range: c(range), ellipsis: c(ell) }
}

/// An independent greedy wrap in characters (the test's own reading of
/// §6.2's construction): the first line holds at most `w - col`.
fn greedy_chars(p: &str, w: usize, col: usize) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut lim = w - col;
    for word in p.split_whitespace() {
        if cur.is_empty() {
            cur = word.to_string();
        } else if cur.chars().count() + 1 + word.chars().count() <= lim {
            cur = format!("{cur} {word}");
        } else {
            out.push(std::mem::take(&mut cur));
            cur = word.to_string();
            lim = w;
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

fn separators(t: &str) -> Vec<String> {
    let m = Measure::of(t);
    (0..m.gaps.len()).map(|i| m.separator(i).to_string()).collect()
}

fn squashed_lines(t: &str) -> Vec<String> {
    t.split('\n').map(text::squash).filter(|l| !l.is_empty()).collect()
}

// ------------------------------------------------------------------ fixtures

#[test]
fn fixture_layout_classes() {
    for (name, t) in [("irc", IRC), ("verse", VERSE), ("transcript", TRANSCRIPT), ("list", LIST), ("talk_page", TALK)] {
        let c = Measure::of(t).counts();
        assert_eq!(c.class, LayoutClass::LineForm, "{name}: {:?}", c.screen_reasons);
        assert!(SourceLayout::whole(t).line_form, "{name}");
    }
    let raid = Measure::of(RAID).counts();
    assert_eq!(raid.class, LayoutClass::Prose);
    assert_eq!((raid.lines, raid.paragraphs, raid.sentence_breaks, raid.blank_line_runs), (4, 4, 3, 0));
    assert_eq!(raid.trailing_ws, BTreeMap::from([("\\s".to_string(), 3)]));
    assert_eq!(raid.tail, "\\n");
    assert_eq!(raid.separators, BTreeMap::from([("\\n".to_string(), 3)]));
    let email = Measure::of(EMAIL).counts();
    assert_eq!(email.class, LayoutClass::HardWrapped);
    assert_eq!(email.width, Some(67));
    assert_eq!((email.lines, email.paragraphs, email.wrap_breaks, email.boundaries), (8, 2, 6, 1));
    let mixed = Measure::of(MIXED_SEPS).counts();
    assert_eq!(mixed.class, LayoutClass::Prose);
    assert_eq!(mixed.paragraphs, 5);
    let m = Measure::of(MIXED_SEPS);
    let seps: Vec<&str> = (0..m.gaps.len()).map(|i| m.separator(i)).collect();
    assert_eq!(seps, vec!["\n\n", "\n", "\n\n\n", "\n \n"]);
    assert!(m.gaps.iter().all(|g| g.boundary()));
    assert_eq!(m.gaps[1].kind, GapKind::SentenceBreak);
    // Amendment 7 item 7.1: a greeting and a sign-off do not make the email line-form,
    // and a short closing line does not make the RAID human line-form.
    let g = Measure::of(EMAIL_GREETING).counts();
    assert_eq!(g.class, LayoutClass::HardWrapped);
    assert_eq!((g.width, g.lines, g.screen_fail_lines, g.passes_screen), (Some(67), 11, 3, false));
    assert_eq!(g.line_breaks, 1, "Thanks, / Sam");
    let r = Measure::of(RAID_SHORT_LAST).counts();
    assert_eq!(r.class, LayoutClass::Prose);
    assert_eq!((r.lines, r.screen_fail_lines, r.passes_screen), (5, 1, false));
    assert_eq!(r.separators, BTreeMap::from([("\\n".to_string(), 4)]));
    // W is in characters (columns) and is the longest wrapped line, not the first.
    let u = Measure::of(WRAPPED_UNICODE).counts();
    assert_eq!(u.class, LayoutClass::HardWrapped);
    assert_eq!(u.width, Some(47));
    assert_eq!((u.wrap_breaks, u.boundaries, u.paragraphs), (6, 1, 2));
    let longest_bytes = WRAPPED_UNICODE.lines().map(str::len).max().unwrap();
    assert_eq!(longest_bytes, 50, "byte widths differ from character widths");
    // Line-form is decided from every paragraph: the prose comes first.
    let pl = Measure::of(PROSE_THEN_LIST).counts();
    assert_eq!(pl.class, LayoutClass::LineForm);
    assert_eq!((pl.lines, pl.screen_fail_lines), (6, 4));
    assert!(Measure::of(PROSE_THEN_LIST).paragraphs[0].reason.is_none());
    // Amendment 10 item 10.1: W counts the two-space lead (amendment 7 read 49,
    // the widest body).
    let ind = Measure::of(INDENTED_WRAPPED).counts();
    assert_eq!((ind.class, ind.width, ind.leading_ws_lines), (LayoutClass::HardWrapped, Some(51), 4));
}

#[test]
fn raid_human_mirror_takes_single_newlines_and_trailing_whitespace() {
    let src = SourceLayout::whole(RAID);
    let t = transfer(CHILD_PROSE, &none(), &src);
    assert_eq!(t.record.path, Path::Prose);
    // The rebuilt paragraphs are v1's at the source's fractions, joined by
    // "\n" with source line j's trailing whitespace, then the tail.
    let r = rebuild_child(CHILD_PROSE, &none(), &src.structure, false);
    let mut want = String::new();
    for (j, p) in r.paragraphs.iter().enumerate() {
        if j > 0 {
            want.push('\n');
        }
        want.push_str(p);
        want.push_str(&src.line_ws[j % src.line_ws.len()].1);
    }
    want.push('\n');
    assert_eq!(t.text, want);
    assert_eq!(r.paragraphs.len(), 4);
    let c = Measure::of(&t.text).counts();
    assert_eq!(c.class, LayoutClass::Prose);
    assert_eq!((c.sentence_breaks, c.blank_line_runs, c.trailing_ws_lines), (3, 0, 3));
    assert_eq!(c.tail, "\\n");
    assert!(t.record.tail_applied && !t.record.emphasis_kept);
    // Emphasis is stripped: the source has none.
    assert!(!t.text.contains("**"));
}

#[test]
fn hard_wrapped_email_mirror_is_wrapped_at_w() {
    let src = SourceLayout::whole(EMAIL);
    assert_eq!(src.width, Some(67));
    let t = transfer(CHILD_PROSE, &none(), &src);
    let r = rebuild_child(CHILD_PROSE, &none(), &src.structure, false);
    let want: Vec<String> = r.paragraphs.iter().map(|p| wrap(p, 67, 0).join("\n")).collect();
    assert_eq!(t.text, format!("{}\n", want.join("\n\n")));
    assert_eq!(t.record.wrapped_at, Some(67));
    for l in t.text.lines() {
        assert!(l.chars().count() <= 67, "{l:?}");
    }
    let c = Measure::of(&t.text).counts();
    assert_eq!(c.class, LayoutClass::HardWrapped);
    assert!(c.width.unwrap() <= 67);
    assert_eq!(c.separators, BTreeMap::from([("\\n\\n".to_string(), 1)]));
    // Unwrapping gives the rebuilt paragraphs back.
    let unwrapped: Vec<String> = t.text.trim_end().split("\n\n").map(text::squash).collect();
    assert_eq!(unwrapped, r.paragraphs);
}

#[test]
fn a_mixed_child_in_a_hard_wrapped_source_starts_at_its_column() {
    let s = EMAIL.find("I read through").unwrap();
    let e = EMAIL.find("need to.").unwrap() + "need to.".len();
    let src = SourceLayout::span(EMAIL, s, e).unwrap();
    assert_eq!(src.width, Some(67));
    assert_eq!(src.column, "Thanks for sending the draft agenda over so quickly. ".len());
    let child = "We went over it on the bus and the plan looks fine to us, but the money part could come first so the finance team can go home early.";
    let t = transfer(child, &none(), &src);
    let doc = splice(EMAIL, (s, e), &t.text).unwrap();
    for l in doc.lines() {
        assert!(l.chars().count() <= 67, "{l:?}");
    }
    assert_eq!(text::squash(&t.text), text::squash(child));
    assert!(!t.text.starts_with(char::is_whitespace) && !t.text.ends_with(char::is_whitespace));
    assert_eq!(Measure::of(&doc).counts().class, LayoutClass::HardWrapped);
}

#[test]
fn mixed_separators_are_used_in_order() {
    let src = SourceLayout::whole(MIXED_SEPS);
    let t = transfer(CHILD_PROSE, &none(), &src);
    assert_eq!(t.layout.realised_breaks, 4);
    let m = Measure::of(&t.text);
    let seps: Vec<&str> = (0..m.gaps.len()).map(|i| m.separator(i)).collect();
    assert_eq!(seps, vec!["\n\n", "\n", "\n\n\n", "\n \n"]);
    assert_eq!(t.record.separators_applied, 4);
    assert_eq!(t.record.separator_fallbacks, 0);
    // A source with fewer boundaries than breaks cycles them.
    let mut st = src.clone();
    st.separators = vec!["\n\n".into(), "\n".into()];
    let t2 = transfer(CHILD_PROSE, &none(), &st);
    let m2 = Measure::of(&t2.text);
    assert_eq!((0..m2.gaps.len()).map(|i| m2.separator(i)).collect::<Vec<_>>(), vec!["\n\n", "\n", "\n\n", "\n"]);
}

#[test]
fn line_form_children_keep_their_own_lines() {
    // IRC: own lines in order, source line j's whitespace, the source's
    // boundary separators in order (all single newlines, so the child's
    // blank line goes: amendment 7 item 7.4), the tail.
    let src = SourceLayout::whole(IRC);
    assert_eq!(separators(IRC), vec!["\n"; 4]);
    assert_eq!(src.separators, vec!["\n".to_string(); 4]);
    let t = transfer(CHILD_LINES, &none(), &src);
    assert_eq!(t.record.path, Path::LineForm);
    assert_eq!(t.text, "<tern> ok the cache is cleared now\n<maple> great, run the build again \n<tern> it passed this time\n<maple> thanks for the help\n");
    assert_eq!((t.record.separators_applied, t.record.separator_fallbacks), (3, 0));
    assert!(!t.text.contains("\n\n"));
    assert!(t.line_reasons.is_empty());
    // Verse: leading whitespace on every other source line; the source's
    // separators in order put the stanza break after the fourth line.
    let v = transfer(CHILD_VERSE, &none(), &SourceLayout::whole(VERSE));
    let want: Vec<String> = squashed_lines(CHILD_VERSE).iter().enumerate().map(|(j, l)| if j % 2 == 1 { format!("  {l}") } else { l.clone() }).collect();
    assert_eq!(v.text, format!("{}\n\n{}\n", want[..4].join("\n"), want[4..].join("\n")));
    assert_eq!(v.record.separators_applied, 7);
    // A child's own blank lines are layout: under the verse source they move
    // to the source's stanza position.
    let shifted = "One line of the song\n\nTwo lines of the song\nThree lines\nFour lines\nFive lines";
    let sv = transfer(shifted, &none(), &SourceLayout::whole(VERSE));
    assert_eq!(separators(&sv.text), vec!["\n", "\n", "\n", "\n\n"]);
    // A line-form source with no boundary (one line) has no blank line: the
    // child's lines are joined by one newline, counted.
    let one = SourceLayout::whole("short line");
    assert!(one.line_form && one.separators.is_empty());
    let o = transfer(CHILD_LINES, &none(), &one);
    assert_eq!(separators(&o.text), vec![LINE_FORM_FALLBACK; 3]);
    assert!(!o.text.contains("\n\n"));
    assert_eq!((o.record.separators_applied, o.record.separator_fallbacks), (0, 3));
    // Transcript, list and talk page: never re-paragraphed or wrapped.
    for src in [TRANSCRIPT, LIST, TALK] {
        for child in [CHILD_LINES, CHILD_VERSE, CHILD_LIST, CHILD_PROSE] {
            let t = transfer(child, &none(), &SourceLayout::whole(src));
            assert_eq!(squashed_lines(&t.text), squashed_lines(&rendered(child)), "{src:.20} / {child:.20}");
            assert_eq!(t.record.wrapped_at, None);
            assert_eq!(t.layout.realised_breaks, 0);
        }
    }
}

/// The child's own lines after the line steps with no rendering: emphasis
/// stripped (no fixture source above uses it).
fn rendered(child: &str) -> String {
    child.replace("**", "")
}

#[test]
fn a_mixed_child_takes_its_span_lines_whitespace() {
    let s = RAID.find("Visitors").unwrap();
    let e = RAID.find("since the flood.").unwrap() + "since the flood.".len();
    let src = SourceLayout::span(RAID, s, e).unwrap();
    assert_eq!(src.separators, vec!["\n".to_string()]);
    assert_eq!(src.line_ws, vec![(String::new(), " ".to_string()), (String::new(), " ".to_string())]);
    assert_eq!((src.structure.pre_words, src.structure.post_words), (0, 0));
    let child = "People lined up from the early morning and the staff gave out small maps at the entrance. The new room shows the harbour maps that sat in storage for years after the flood.";
    let t = transfer(child, &none(), &src);
    // One break at the source's fraction; line 0 keeps its trailing space,
    // the last line none (the parent's own space follows the span).
    assert_eq!(t.text, "People lined up from the early morning and the staff gave out small maps at the entrance. \nThe new room shows the harbour maps that sat in storage for years after the flood.");
    let doc = splice(RAID, (s, e), &t.text).unwrap();
    assert_eq!(Measure::of(&doc).counts().trailing_ws_lines, 3);
}

/// A prose parent with indented single-newline paragraphs, a head and paired
/// emphasis in its second paragraph.
const INDENTED: &str = "\n  The first paragraph of the letter thanks the committee for the invitation to speak.\n  The second paragraph says that the **whole** team will travel down on the Friday train.\n  The third paragraph asks whether a projector will be available in the main hall.";

#[test]
fn a_span_takes_interior_leading_whitespace_and_a_mirror_the_head() {
    let c = Measure::of(INDENTED).counts();
    assert_eq!(c.class, LayoutClass::Prose);
    assert_eq!((c.leading_ws_lines, c.emphasis_pairs, c.head.as_str()), (3, 1, "\\n"));
    // Mirror: the head, every line's indent, and the source's emphasis kept.
    let child = "Thanks for asking us to come and talk to the group next month. All **four** of us can come on the early train that Friday. Could you tell us if there is a screen we can use in the hall?";
    let m = transfer(child, &none(), &SourceLayout::whole(INDENTED));
    assert!(m.record.head_applied && m.record.emphasis_kept);
    assert!(m.text.starts_with("\n  Thanks"), "{:?}", &m.text[..12]);
    assert!(m.text.contains("**four**"));
    assert_eq!(m.text.matches("\n  ").count(), 3);
    // Span over the second and third paragraphs: the child's first line takes
    // no indent (the parent's own bytes before the span hold it), the next
    // line takes the third line's.
    let s = INDENTED.find("The second").unwrap();
    let e = INDENTED.len();
    let src = SourceLayout::span(INDENTED, s, e).unwrap();
    assert_eq!(src.line_ws, vec![("  ".to_string(), String::new()), ("  ".to_string(), String::new())]);
    assert!(src.emphasis);
    let child2 = "The **whole** group will come down on the Friday morning train together. They would also like to know if a projector can be set up in the hall.";
    let t = transfer(child2, &none(), &src);
    assert_eq!(t.text, "The **whole** group will come down on the Friday morning train together.\n  They would also like to know if a projector can be set up in the hall.");
    let doc = splice(INDENTED, (s, e), &t.text).unwrap();
    assert_eq!(Measure::of(&doc).counts().leading_ws_lines, 3);
    // Without emphasis in the source region, the child's is stripped.
    let s3 = INDENTED.find("The third").unwrap();
    let t3 = transfer(child2, &none(), &SourceLayout::span(INDENTED, s3, e).unwrap());
    assert!(!t3.text.contains("**"));
}

#[test]
fn a_passing_source_screens_and_echo_applies_always() {
    let conv = none();
    // A list child under a passing prose source (empty licence) fails the
    // form screen...
    let prose = child_v2(RAID, None, &conv, CHILD_LIST);
    assert_eq!(prose.record["form_screen_applied"], true);
    assert!(prose.reasons.contains(&"form_line_list_like".to_string()), "{:?}", prose.reasons);
    // ...and passes it under a line-form source, where the screen is off.
    let lined = child_v2(LIST, None, &conv, CHILD_LIST);
    assert_eq!(lined.record["form_screen_applied"], false);
    assert!(lined.reasons.is_empty(), "{:?}", lined.reasons);
    // The echo screen applies to every child: a mirror equal to its parent.
    let echo = child_v2(IRC, None, &conv, IRC);
    assert!(echo.reasons.contains(&"echo_mirror_byte_identical".to_string()), "{:?}", echo.reasons);
    // A span equal to the replaced span, raw.
    let s = RAID.find("Visitors").unwrap();
    let e = RAID.find("at the door.").unwrap() + "at the door.".len();
    let span = child_v2(RAID, Some((s, e)), &conv, &RAID[s..e]);
    assert!(span.reasons.contains(&"echo_span_byte_identical".to_string()), "{:?}", span.reasons);
    // A good child passes both.
    let ok = child_v2(RAID, None, &conv, CHILD_PROSE);
    assert!(ok.reasons.is_empty(), "{:?}", ok.reasons);
    // Spans outside the parent fail closed.
    let bad = child_v2(RAID, Some((5, 3)), &conv, CHILD_PROSE);
    assert!(bad.reasons.contains(&"form_span_outside_parent".to_string()) && bad.reasons.contains(&"echo_span_outside_parent".to_string()));
}

// ------------------------------------------------------------------ v2 = v1

/// Excerpt-layout parents that pass the screen and use no paired emphasis.
fn excerpt_parents() -> Vec<String> {
    let mut out: Vec<String> = vec![
        "\u{201C}We should leave before the tide turns,\u{201D} the warden told the group on the pier.\n\nThe boats were tied up early that evening\u{2014}nobody wanted to risk the channel after dark.\n\nBy morning the water was calm again, and the ferry ran on time from 7\u{2013}9 as usual.".into(),
        "\"It's fine,\" said the clerk - mostly to himself - as he stamped the forms on the desk.\n\nThe office opened at 9-5 on weekdays, and the queue grew longer every hour... nobody complained aloud.\n\nHe filed the last folder at noon and went out for a sandwich at the corner shop.".into(),
        "The lecture ran long \u{2013} as lectures do \u{2013} and the room emptied slowly after six, with pages 10\u{2013}12 set for the following week.".into(),
        "Dr. Hale met Mr. Ortiz at 3 p.m. on Jan. 4 near the old border post. They spoke briefly about the permits.\n\nJ. Ortiz later wrote that the talk went well. Prof. Lin agreed with him on most points.\n\nThe permits arrived a month later, in a thick envelope with a short covering letter.\n\nNobody mentioned the meeting again until the following spring, when the river flooded.".into(),
    ];
    out.push(excerpt_layout(MIXED_SEPS));
    out.push(excerpt_layout(RAID));
    for p in &out {
        let c = Measure::of(p).counts();
        assert_eq!(c.class, LayoutClass::Excerpt, "{p:.30}");
        assert_eq!(c.emphasis_pairs, 0);
    }
    out
}

fn children() -> Vec<String> {
    let mut out: Vec<String> = vec![CHILD_PROSE.into(), CHILD_LIST.into(), CHILD_LINES.into(), CHILD_VERSE.into()];
    out.push("# A Day at the Harbour\n\nThe ferry left at dawn.\nThe gulls followed it out to sea for a while, then turned back to land. It was a quiet trip. Nobody minded the wait at all.".into());
    out.push("\u{201C}It\u{2019}s late,\u{201D} she said. \u{2018}Very late.\u{2019} We left at 9\u{2013}10 p.m. and walked home\u{2026} The streets were empty and the shops had closed hours before.".into());
    out.push("Short.".into());
    out.push(String::new());
    out.push("The report came in late.\r\nIt was\u{a0}long and\tdetailed, with many tables.\r\n\r\nThe board read it twice before the vote on Friday afternoon.".into());
    let mut long = String::new();
    for i in 0..40 {
        let n = 3 + (i * 7) % 13;
        let words: Vec<String> = (0..n).map(|k| format!("w{i}x{k}")).collect();
        long.push_str(&format!("{} {}{}", if i % 5 == 0 { "And" } else { "The" }, words.join(" "), if i % 9 == 0 { "? " } else { ". " }));
    }
    out.push(long.trim_end().to_string());
    out
}

fn spans_of(p: &str) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut start = 0;
    for (i, _) in p.match_indices("\n\n") {
        out.push((start, i));
        start = i + 2;
    }
    out.push((start, p.len()));
    if let (Some(a), Some(b)) = (p.find(". "), p.rfind(". "))
        && a + 2 <= b + 1
    {
        out.push((a + 2, b + 1));
    }
    out.retain(|&(s, e)| s < e && p.is_char_boundary(s) && p.is_char_boundary(e));
    out
}

/// v1's EraPost form verdict (normalise, line reasons, the paragraph screen
/// on the assembled document's generated paragraphs).
fn v1_form(parent: &str, span: Option<(usize, usize)>, conv: &Conventions, child: &str) -> (String, Vec<String>) {
    let st = match span {
        Some((s, e)) => Structure::span(parent, s, e),
        None => Structure::whole(parent),
    };
    let n = normalise_child(child, conv, &st);
    let mut r = n.line_reasons.clone();
    if n.text.is_empty() {
        r.push("form_empty".into());
    } else {
        let assembled = match span {
            Some(sp) => splice(parent, sp, &n.text).map(|t| (t, vec![(sp.0, sp.0 + n.text.len())])),
            None => Ok((n.text.clone(), vec![(0, n.text.len())])),
        };
        match assembled {
            Ok((t, fin)) => r.extend(paragraph_screen(&t, Some(fin.as_slice()))),
            Err(e) => r.push(format!("form_assembly_error:{e}")),
        }
    }
    r.sort();
    r.dedup();
    (n.text, r)
}

#[test]
fn v2_is_byte_identical_to_v1_on_excerpt_layout_parents() {
    let mut cases = 0;
    for parent in excerpt_parents() {
        let convs = vec![
            resolve(&counts(&parent), &Prior::default(), "s", "d"),
            none(),
            fixed(Quote::Curly, Dash::EmClosed, Range::En, Ellipsis::Char),
            fixed(Quote::Ascii, Dash::HyphenSpaced, Range::Hyphen, Ellipsis::Dots),
        ];
        let mut spans: Vec<Option<(usize, usize)>> = vec![None];
        spans.extend(spans_of(&parent).into_iter().map(Some));
        for conv in &convs {
            for child in children() {
                for &span in &spans {
                    let (st, src) = match span {
                        None => (Structure::whole(&parent), SourceLayout::whole(&parent)),
                        Some((s, e)) => (Structure::span(&parent, s, e), SourceLayout::span(&parent, s, e).unwrap()),
                    };
                    assert_eq!(src.structure, st);
                    let v1 = normalise_child(&child, conv, &st);
                    let v2 = transfer(&child, conv, &src);
                    assert_eq!(v2.text, v1.text, "parent {parent:.30} child {child:.30} span {span:?}");
                    assert_eq!(v2.line_reasons, v1.line_reasons);
                    assert_eq!(v2.changes, v1.changes);
                    assert_eq!(serde_json::to_value(&v2.layout).unwrap(), serde_json::to_value(&v1.layout).unwrap());
                    let (t1, r1) = v1_form(&parent, span, conv, &child);
                    let c2 = child_v2(&parent, span, conv, &child);
                    assert_eq!(c2.text, t1);
                    let form2: Vec<String> = c2.record["form_reasons"].as_array().unwrap().iter().map(|x| x.as_str().unwrap().to_string()).collect();
                    assert_eq!(form2, r1, "parent {parent:.30} child {child:.30} span {span:?}");
                    cases += 1;
                }
            }
        }
    }
    assert!(cases > 400, "{cases}");
}

// ------------------------------------------------------------------ records

fn strings(v: &Value, out: &mut Vec<String>) {
    match v {
        Value::String(s) => out.push(s.clone()),
        Value::Array(a) => a.iter().for_each(|x| strings(x, out)),
        Value::Object(o) => {
            for (k, x) in o {
                out.push(k.clone());
                strings(x, out);
            }
        }
        _ => {}
    }
}

fn assert_no_text(rec: &Value, texts: &[&str]) {
    let mut ss = Vec::new();
    strings(rec, &mut ss);
    for s in &ss {
        // Ids, reason codes, enum values, digests and escaped whitespace; a
        // donor's layout also holds raw whitespace strings.
        assert!(
            s.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || "_:\\{}".contains(c))
                || s.chars().all(char::is_whitespace)
                || s.starts_with("form_assembly_error:"),
            "record string {s:?}"
        );
    }
    for t in texts {
        for l in t.split('\n').map(str::trim).filter(|l| l.chars().count() >= 12) {
            assert!(!ss.iter().any(|s| s.contains(l)), "record holds text {l:?}");
        }
        for w in t.split_whitespace().filter(|w| w.len() >= 7 && w.chars().all(|c| c.is_ascii_alphabetic())) {
            let w = w.to_lowercase();
            assert!(!ss.contains(&w), "record holds the word {w:?}");
        }
    }
}

#[test]
fn records_hold_no_text() {
    let conv = resolve(&counts(RAID), &Prior::default(), "s", "d");
    for parent in [IRC, VERSE, TRANSCRIPT, LIST, TALK, RAID, EMAIL, MIXED_SEPS] {
        for child in [CHILD_PROSE, CHILD_LINES, CHILD_VERSE, CHILD_LIST] {
            let c = child_v2(parent, None, &conv, child);
            assert_no_text(&c.record, &[parent, child]);
            assert_eq!(c.record["profile"], PROFILE_V2);
            assert_eq!(c.record["raw_response_sha256"].as_str().unwrap().len(), 64);
            assert!(c.record["source"]["class"].is_string() && c.record["source"]["skeleton_sha256"].is_string());
        }
    }
    let s = RAID.find("Visitors").unwrap();
    let e = RAID.find("at the door.").unwrap() + "at the door.".len();
    let c = child_v2(RAID, Some((s, e)), &conv, CHILD_PROSE);
    assert_no_text(&c.record, &[RAID, CHILD_PROSE]);
    // The source layout of a donor is text-free too.
    let donor = serde_json::to_value(SourceLayout::whole(EMAIL)).unwrap();
    assert_no_text(&donor, &[EMAIL]);
}

// ------------------------------------------------------------------ prior

#[test]
fn population_by_domain_prior_and_its_fallbacks() {
    let news_a = "\u{201C}Yes,\u{201D} the mayor said \u{2014} twice \u{2014} at the meeting.";
    let news_b = "\u{201C}No,\u{201D} the clerk replied \u{2014} quietly \u{2014} to the room.";
    let blog = "\"Sure,\" I said, and we left at 9-5 on a Friday... as always.";
    let p = PriorV2::of("train2", [("news", news_a), ("news", news_b), ("blog", blog)].into_iter());
    assert_eq!((p.population.as_str(), p.prior.parents), ("train2", 3));
    assert_eq!(p.by_domain["news"].parents, 2);
    assert_eq!(p.by_domain["news"].dash, BTreeMap::from([(Dash::EmSpaced, 2)]));
    assert!(p.by_domain["blog"].dash.is_empty());
    let plain = counts("Plain words with no quotes or dashes at all here.");
    for k in 0..32 {
        let doc = format!("doc-{k}");
        let n = resolve_v2(&plain, &p, "news", "seed-a", &doc);
        assert_eq!(n.dash, Choice { value: Some(Dash::EmSpaced), source: "keyed_prior_domain" });
        assert_eq!(n.double, Choice { value: Some(Quote::Curly), source: "keyed_prior_domain" });
        // The blog cell shows no dash: the population-wide prior draws.
        let b = resolve_v2(&plain, &p, "blog", "seed-a", &doc);
        assert_eq!(b.dash.source, "keyed_prior_population");
        assert_eq!(b.dash.value, Some(Dash::EmSpaced));
        assert_eq!(b.range, Choice { value: Some(Range::Hyphen), source: "keyed_prior_domain" });
        // No parent anywhere shows the convention: none (v1's default).
        let news_range = resolve_v2(&plain, &p, "news", "seed-a", &doc);
        assert_eq!(news_range.range.source, "keyed_prior_population");
        let empty = PriorV2::of("cal2", std::iter::empty());
        assert_eq!(resolve_v2(&plain, &empty, "news", "seed-a", &doc).dash, Choice { value: None, source: "none" });
        // An unknown domain draws from the population.
        assert_eq!(resolve_v2(&plain, &p, "legal", "seed-a", &doc).dash.source, "keyed_prior_population");
        // The parent's own majority wins over any prior.
        assert_eq!(resolve_v2(&counts(blog), &p, "news", "seed-a", &doc).range, Choice { value: Some(Range::Hyphen), source: "parent" });
        assert_eq!(resolve_v2(&counts(news_a), &p, "blog", "seed-a", &doc).dash, Choice { value: Some(Dash::EmSpaced), source: "parent" });
        // Deterministic per seed and document.
        assert_eq!(n, resolve_v2(&plain, &p, "news", "seed-a", &doc));
    }
    // With one domain the draws are v1's on the same (population) prior.
    let one = PriorV2::of("x", [("d", news_a), ("d", blog)].into_iter());
    for k in 0..64 {
        let doc = format!("doc-{k}");
        let a = resolve_v2(&plain, &one, "d", "seed-b", &doc);
        let b = resolve(&plain, &one.prior, "seed-b", &doc);
        assert_eq!((a.double.value, a.dash.value, a.range.value, a.ellipsis.value), (b.double.value, b.dash.value, b.range.value, b.ellipsis.value));
    }
    // Seeds differ per population: some draw changes.
    let q = PriorV2::of("y", [("d", news_a), ("d", blog)].into_iter());
    let diff = (0..64).any(|k| resolve_v2(&plain, &q, "d", "seed-1", &format!("doc-{k}")).double.value != resolve_v2(&plain, &q, "d", "seed-2", &format!("doc-{k}")).double.value);
    assert!(diff);
}

// ------------------------------------------------------------------ profile

#[test]
fn profile_v2_id_window_and_every_class() {
    assert_eq!(Profile::parse(PROFILE_V2).unwrap(), Profile::CohortV2);
    assert_eq!(Profile::CohortV2.id(), "cc_cohort_profile_v2");
    assert_eq!(Profile::CohortV2.window(), Some((150, 450)));
    assert!(Profile::CohortV2.excludes() && Profile::CohortV2.normalises() && Profile::CohortV2.transfers());
    assert!(!Profile::CohortV1.transfers());
    assert_eq!(Profile::DEFAULT, Profile::CohortV1);
    assert!(Profile::CohortV2.check_prior(None).is_err());
    let rec = Profile::CohortV2.record();
    assert_eq!(rec["layout_transfer"], "layout_transfer_v4");
    assert_eq!(rec["layout_measure"], "layout_measure_v2");
    assert_eq!(rec["echo_screen"], "cc_echo_screen_v1");
    // The C block keeps v1's every-class step.
    let para = "The ferry left the harbour at dawn and the gulls followed it out to sea for a while.";
    for n in [8usize, 9, 25, 26] {
        let doc = vec![para; n].join("\n\n");
        let (t1, r1, w1) = every_class(&doc, Profile::CohortV1);
        let (t2, r2, w2) = every_class(&doc, Profile::CohortV2);
        assert_eq!((t1, w1), (t2, w2));
        assert_eq!(r2["profile"], PROFILE_V2);
        assert_eq!(r1["length_window"], r2["length_window"]);
        assert_eq!(r1["excluded"], r2["excluded"]);
    }
    // The T block: the window only, the text never laid out.
    let raw = format!("{}  \n{}", [para; 5].join("\n"), [para; 5].join("\n"));
    let (rec, why) = every_class_t_block(&raw);
    assert!(why.is_empty() && rec["excluded"] == false && rec["block"] == "T");
    let (_, short) = every_class_t_block(para);
    assert_eq!(short, vec!["final_document_below_150_words".to_string()]);
}

#[test]
fn splice_follows_the_assembly_rules() {
    let p = "Alpha one. Beta two.";
    assert_eq!(splice(p, (11, 20), "Gamma three.").unwrap(), "Alpha one. Gamma three.");
    assert_eq!(splice(p, (11, 11), "x"), Err("span order"));
    assert_eq!(splice(p, (12, 20), "x"), Err("left token boundary"));
    assert_eq!(splice(p, (11, 19), "x"), Err("right token boundary"));
    assert_eq!(splice(p, (11, 20), " x"), Err("replacement layout"));
}

// ------------------------------------------------------------------ amendments 7 and 10

#[test]
fn a_greeting_and_sign_off_keep_the_email_prose_and_wrapped() {
    // Amendment 7 item 7.1 (r2 F1, review finding 1): the mirror is
    // re-paragraphed and wrapped at the parent's W. Amendment 10 items
    // 10.4-10.5 (problem 1): the greeting and the sign-off are the source's
    // edge frames, its body licence is empty (the body passes the screen),
    // and the licence screen applies; amendment 7 left the screen off.
    let src = SourceLayout::whole(EMAIL_GREETING);
    assert!(!src.line_form && !src.passes_screen);
    assert_eq!(src.width, Some(67));
    assert!(src.frames.head && src.frames.tail && src.licence.is_empty());
    assert_eq!((src.frames.head_seps.clone(), src.frames.tail_seps.clone()), (vec!["\n\n".to_string()], vec!["\n\n".to_string(), "\n".to_string()]));
    let c = child_v2(EMAIL_GREETING, None, &none(), CHILD_PROSE);
    assert!(c.reasons.is_empty(), "{:?}", c.reasons);
    assert_eq!(c.record["transfer"]["path"], "prose");
    assert_eq!(c.record["transfer"]["wrapped_at"], 67);
    assert_eq!(c.record["form_screen_applied"], true);
    assert_eq!(c.record["source"]["class"], "hard_wrapped");
    for l in c.text.lines() {
        assert!(l.chars().count() <= 67, "{l:?}");
    }
    // The child has no short edge paragraph, so no frame: its two body
    // paragraphs (the source body's one break) are joined by the source's
    // body boundary, and the parent's tail follows.
    assert_eq!(src.separators, vec!["\n\n"]);
    let m = Measure::of(&c.text);
    let seps: Vec<String> = (0..m.gaps.len()).filter(|&i| m.gaps[i].boundary()).map(|i| m.separator(i).to_string()).collect();
    assert_eq!(seps, vec!["\n\n"], "{}", c.text);
    assert_eq!(c.record["transfer"]["separators_applied"], 1);
    // A child with its own greeting and sign-off keeps them as frame lines,
    // with the source's frame-side separators.
    let framed = format!("Hello Dana,\n\n{CHILD_PROSE}\n\nBest,\nSam");
    let f = child_v2(EMAIL_GREETING, None, &none(), &framed);
    assert!(f.reasons.is_empty(), "{:?}", f.reasons);
    assert!(f.text.starts_with("Hello Dana,\n\n") && f.text.ends_with("\n\nBest,\nSam\n"), "{:?}", f.text);
    // A mixed child in the body: its region passes, so it gets §6.2's screen.
    let s = EMAIL_GREETING.find("I also spoke").unwrap();
    let e = EMAIL_GREETING.find("last year?").unwrap() + "last year?".len();
    let mixed = child_v2(EMAIL_GREETING, Some((s, e)), &none(), "We also asked the venue about a projector, and they can lend one for the afternoon if we bring cables.");
    assert_eq!(mixed.record["form_screen_applied"], true);
    assert!(mixed.reasons.is_empty(), "{:?}", mixed.reasons);
    let doc = splice(EMAIL_GREETING, (s, e), &mixed.text).unwrap();
    assert!(doc.lines().all(|l| l.chars().count() <= 67));
}

#[test]
fn a_short_closing_line_keeps_the_raid_human_prose() {
    // Review finding 1, probe P9: the mirror's separators are the parent's
    // single newlines, never a blank line.
    let c = child_v2(RAID_SHORT_LAST, None, &none(), CHILD_PROSE);
    assert_eq!(c.record["transfer"]["path"], "prose");
    assert!(!c.text.contains("\n\n"), "{:?}", separators(&c.text));
    assert!(separators(&c.text).iter().all(|x| x == "\n"));
    assert!(separators(&c.text).len() >= 2);
}

#[test]
fn a_span_takes_the_whole_parent_width() {
    // Amendment 7 item 7.3 (review finding 3b, probe P6): a one-line paragraph
    // inside a hard-wrapped parent is rewritten wrapped at the parent's W.
    let parent = format!("{EMAIL}\nSee you all at the meeting on Friday morning in the main room.\n");
    let s = parent.find("See you").unwrap();
    let e = parent.find("main room.").unwrap() + "main room.".len();
    let src = SourceLayout::span(&parent, s, e).unwrap();
    assert_eq!(src.width, Some(67));
    assert_eq!(src.counts.class, LayoutClass::HardWrapped);
    let child = "We will all meet on Friday morning in the main room, and the finance people can leave as soon as the budget item is done, which should be early.";
    assert!(child.chars().count() > 67);
    let c = child_v2(&parent, Some((s, e)), &none(), child);
    assert_eq!(c.record["transfer"]["wrapped_at"], 67);
    let doc = splice(&parent, (s, e), &c.text).unwrap();
    assert!(doc.lines().all(|l| l.chars().count() <= 67), "{doc}");
    assert_eq!(c.text.lines().map(String::from).collect::<Vec<_>>(), greedy_chars(child, 67, 0));
}

#[test]
fn titles_lead_ins_and_signatures_are_not_hard_wraps() {
    // Amendment 7 item 7.2 (review finding 3a, probes P2 and P3).
    let titled = "A Walk by the River\nWe walked along the river path until the light began to fade behind the hills.\nThe dogs ran ahead of us and waited at every gate along the way home.";
    let lead = "Here is what we agreed at the meeting:\nWe will move the budget item to the top of the agenda next week.\nThe venue will provide a projector for the afternoon session.";
    let signed = "The report is attached and the figures for the third quarter are in the second table of it.\n\nSam Smith\nProject Manager, Gas Trading\nNorth Office";
    for t in [titled, lead, signed] {
        let c = Measure::of(t).counts();
        assert!(!c.hard_wrapped && c.width.is_none(), "{t:.20}");
        let m = transfer(CHILD_PROSE, &none(), &SourceLayout::whole(t));
        assert_eq!(m.record.wrapped_at, None);
        assert!(m.text.lines().any(|l| l.chars().count() > 70), "not wrapped");
    }
    // The title is its own paragraph: the child's paragraphs are joined by
    // the parent's single newlines.
    let c = Measure::of(titled).counts();
    assert_eq!((c.class, c.paragraphs, c.line_breaks, c.sentence_breaks), (LayoutClass::Prose, 3, 1, 1));
    let m = transfer(CHILD_PROSE, &none(), &SourceLayout::whole(titled));
    assert!(!m.text.contains("\n\n"));
}

#[test]
fn verse_is_line_form_unless_a_greedy_wrapper_could_have_made_it() {
    // Amendment 7 item 7.2 (review finding 3c): the stanzas end a sentence,
    // but a short line's next word would have fit, so the breaks are line
    // breaks and the lines are their own (failing) paragraphs.
    let lines_only = "The cat sat still,\nthe wide grey sea was rolling in.\nThe birds went home,\nthe night came down upon the town.";
    let c = Measure::of(lines_only).counts();
    assert_eq!((c.class, c.hard_wrapped), (LayoutClass::LineForm, false));
    let v = transfer(CHILD_VERSE, &none(), &SourceLayout::whole(lines_only));
    assert_eq!(v.record.path, Path::LineForm);
    assert_eq!(squashed_lines(&v.text), squashed_lines(CHILD_VERSE));
    // Changed by amendment 10 (items 10.1 and 10.2(d)): verse whose
    // alternate lines are indented. With columns, the first stanza's
    // indented short lines pass the fit test (30 + 1 + 3 > 33), so its
    // breaks are first-pass evidence (W 33); the unindented lines differ
    // from the continuation lead (two spaces) and start paragraphs.
    // Amendment 7 read it line-form. Amendment 10 read the second stanza's
    // lines, 1-2 columns wider than W, as unwrapped (six paragraphs, four
    // failing lines without a wrap); under amendment 13 item 13.5 a line
    // within one word of W is not unwrapped, so the second stanza reads like
    // the first: four wrapped paragraphs (a stated limit with a B12 count,
    // item 13.10).
    let uneven = "The lamps are lit along the quay,\n  the boats are drawn up high,\nthe nets are hung to dry tonight,\n  and gulls go wheeling by.\n\nThe tide comes in across the sand,\n  the wind is soft and low,\nthe harbour bell rings out at nine,\n  and home the sailors go.";
    let u = Measure::of(uneven).counts();
    assert_eq!((u.class, u.width, u.paragraphs, u.line_form_fail_lines), (LayoutClass::HardWrapped, Some(33), 4, 0));
    assert_eq!(Measure::of_rule(uneven, grammar_a_form::measure::WidthRule::Body).counts().class, LayoutClass::LineForm);
    // Stated limit (amendment 7 item 7.2): verse whose every break a greedy
    // wrapper could have made, in stanzas that pass the screen, is read as
    // hard-wrapped prose.
    let even = "The woods are quiet, dark and deep,\nthe path runs on beneath the hill,\nand all the birds have gone to sleep.\n\nThe moon comes up above the mill,\nthe gate is shut, the yard is still,\nand no one walks the lane tonight.";
    let e = Measure::of(even).counts();
    // W 37: the evidence runs' last lines count (amendment 13 item 13.5).
    assert_eq!((e.class, e.width), (LayoutClass::HardWrapped, Some(37)));
}

#[test]
fn widths_and_columns_are_characters_of_the_line() {
    // Review finding 5: a mirror is wrapped at W in characters...
    let src = SourceLayout::whole(WRAPPED_UNICODE);
    assert_eq!(src.width, Some(47));
    let child = "The caf\u{e9} owners met again on Tuesday \u{2014} na\u{ef}vely, some said \u{2014} and agreed to keep the shops open late. The mayor\u{2019}s office did not reply to their letter at all.";
    let t = transfer(child, &none(), &src);
    let r = rebuild_child(child, &none(), &src.structure, false);
    let want: Vec<String> = r.paragraphs.iter().map(|p| greedy_chars(p, 47, 0).join("\n")).collect();
    assert_eq!(t.text, format!("{}\n", want.join("\n\n")));
    assert!(t.text.lines().any(|l| l.len() > 47 && l.chars().count() <= 47), "a line whose bytes exceed W");
    // ...a span's first line starts at its column in characters...
    let s = WRAPPED_UNICODE.find("quay").unwrap();
    let e = WRAPPED_UNICODE.find("them all.").unwrap() + "them all.".len();
    let sp = SourceLayout::span(WRAPPED_UNICODE, s, e).unwrap();
    assert_eq!((sp.column, s), (19, 20));
    let c = "The quay folk said the rules were unfair to the small caf\u{e9}s on the front \u{2014} and asked the council to think again.";
    assert_eq!(transfer(c, &none(), &sp).text, greedy_chars(c, 47, 19).join("\n"));
    // ...counting the line's leading whitespace (amendment 10 item 10.1:
    // columns; amendment 7 counted the body only, column 30 at W 49). The
    // continuation lines take the two-space lead and a budget of W - 2, so
    // the body layout is unchanged.
    let s = INDENTED_WRAPPED.find("hall").unwrap();
    let e = INDENTED_WRAPPED.find("Friday.").unwrap() + "Friday.".len();
    let ind = SourceLayout::span(INDENTED_WRAPPED, s, e).unwrap();
    assert_eq!((ind.column, ind.width), ("  We were glad to hear that the ".len(), Some(51)));
    let c2 = "main hall is booked for the whole afternoon on the first Saturday, and we will send the cost by Friday.";
    let lines = greedy_chars(c2, 49, ind.column - 2);
    assert_eq!(lines[0], "main hall is booked");
    assert_eq!(transfer(c2, &none(), &ind).text, lines.join("\n  "));
}

#[test]
fn the_screen_is_the_span_regions_in_a_line_form_parent() {
    // Review finding 5: a span in a prose paragraph of a line-form parent is
    // laid out as prose and screened, because its own paragraphs pass.
    assert!(SourceLayout::whole(PROSE_THEN_LIST).line_form);
    let s = 0;
    let e = PROSE_THEN_LIST.find("spring walks.").unwrap() + "spring walks.".len();
    let src = SourceLayout::span(PROSE_THEN_LIST, s, e).unwrap();
    assert!(src.passes_screen && !src.line_form);
    let bad = child_v2(PROSE_THEN_LIST, Some((s, e)), &none(), "- the club met on Monday\n- the plan was agreed");
    assert_eq!(bad.record["form_screen_applied"], true);
    assert!(bad.reasons.contains(&"form_line_list_like".to_string()), "{:?}", bad.reasons);
    // A span over the list is line-form and not screened.
    let ls = PROSE_THEN_LIST.find("- a waterproof").unwrap();
    let le = PROSE_THEN_LIST.trim_end().len();
    let lst = child_v2(PROSE_THEN_LIST, Some((ls, le)), &none(), "- a warm hat\n- spare socks");
    assert_eq!((lst.record["form_screen_applied"].clone(), lst.record["transfer"]["path"].clone()), (Value::Bool(false), Value::from("line_form")));
    assert!(lst.reasons.is_empty(), "{:?}", lst.reasons);
}

#[test]
fn step_four_governs_emphasis_on_excerpt_parents() {
    // Amendment 7 item 7.7 (review finding 6): an excerpt-layout parent that
    // uses paired emphasis keeps the child's, where v1 strips it.
    let parent = "The council approved the **new** parking plan for the town centre on Tuesday.\n\nResidents will see the first changes on the high street early next month.";
    assert_eq!(Measure::of(parent).counts().class, LayoutClass::Excerpt);
    let child = "The council backed the **revised** plan for parking in the centre of town this week.\n\nThe first changes will reach the high street at the start of next month.";
    let v2 = child_v2(parent, None, &none(), child);
    let v1 = normalise_child(child, &none(), &Structure::whole(parent));
    assert!(v2.text.contains("**revised**") && !v1.text.contains("**"));
    assert_eq!(v2.text.replace("**", ""), v1.text);
    // Without paired emphasis in the child, the two agree.
    let plain = child.replace("**", "");
    assert_eq!(child_v2(parent, None, &none(), &plain).text, normalise_child(&plain, &none(), &Structure::whole(parent)).text);
}

// ------------------------------------------------------------------ items

/// A prose parent with three paragraphs for spliced items.
const ITEM_PARENT: &str = "The museum reopened last week after two years of repairs. Visitors queued from early morning.\n\nThe new gallery holds the old maps of the harbour. Most people stayed for an hour.\n\nThe cafe ran out of sandwiches well before noon on the first day.";

#[test]
fn item_v2_assembles_insertions_and_several_edits() {
    // Review finding 4: an insertion (s == e, with its separator) and a
    // replacement in one item, assembled by the spliced rules and screened
    // on the assembled item.
    let ins = ITEM_PARENT.find("Visitors").unwrap() - 1;
    let rs = ITEM_PARENT.find("Most people").unwrap();
    let re = ITEM_PARENT.find("for an hour.").unwrap() + "for an hour.".len();
    let edits = [
        EditV2 { span: (ins, ins), insert_sep: Some(" "), raw: "The roof had leaked for many winters before the work began." },
        EditV2 { span: (rs, re), insert_sep: None, raw: "Many visitors spent the whole afternoon looking at them." },
    ];
    let it = item_v2(ITEM_PARENT, &none(), &edits);
    assert!(it.reasons.is_empty(), "{:?}", it.reasons);
    let a = it.assembled.as_ref().unwrap();
    let want = assemble_item(ITEM_PARENT, &[Part { span: (ins, ins), insert_sep: Some(" ") }, Part { span: (rs, re), insert_sep: None }], &[&it.reps[0], &it.reps[1]]).unwrap();
    assert_eq!(a, &want);
    assert_eq!(&a.text[a.edit_spans[0].0..a.edit_spans[0].1], edits[0].raw);
    assert!(a.text.contains("repairs. The roof had leaked"));
    assert_eq!(it.record["edits"][0]["insertion"], true);
    assert_eq!(it.record["edits"][0]["insert_sep"], "\\s");
    assert_eq!(it.record["echo"]["edits"], 2);
    assert_eq!(it.record["edits"][0]["form_screen_applied"], true);
    // An insertion at the end in a new paragraph.
    let end = ITEM_PARENT.len();
    let tail = item_v2(ITEM_PARENT, &none(), &[EditV2 { span: (end, end), insert_sep: Some("\n\n"), raw: "The gallery will stay open late on Fridays from next month onwards." }]);
    assert!(tail.reasons.is_empty(), "{:?}", tail.reasons);
    assert!(tail.assembled.unwrap().text.ends_with("first day.\n\nThe gallery will stay open late on Fridays from next month onwards."));
    // child_v2 refuses an insertion (generation::assemble's spans); item_v2
    // without a separator fails like assemble_parts.
    let c = child_v2(ITEM_PARENT, Some((ins, ins)), &none(), edits[0].raw);
    assert!(c.reasons.contains(&"form_assembly_error:span order".to_string()), "{:?}", c.reasons);
    let nosep = item_v2(ITEM_PARENT, &none(), &[EditV2 { span: (ins, ins), insert_sep: None, raw: edits[0].raw }]);
    assert!(nosep.reasons.contains(&"form_assembly_error:insert separator".to_string()), "{:?}", nosep.reasons);
}

#[test]
fn item_v2_screens_the_assembled_item_and_echoes_every_edit() {
    let ins = ITEM_PARENT.find("Visitors").unwrap() - 1;
    let rs = ITEM_PARENT.find("Most people").unwrap();
    let re = ITEM_PARENT.find("for an hour.").unwrap() + "for an hour.".len();
    // The second edit is a list line: its own line reason, from the item.
    let it = item_v2(ITEM_PARENT, &none(), &[
        EditV2 { span: (ins, ins), insert_sep: Some(" "), raw: "The roof had leaked for many winters before the work began." },
        EditV2 { span: (rs, re), insert_sep: None, raw: "- a list line" },
    ]);
    assert!(it.reasons.contains(&"form_line_list_like".to_string()), "{:?}", it.reasons);
    // A paragraph of the assembled item that fails classify: an insertion
    // that ends the item's last paragraph without a sentence end.
    let end = ITEM_PARENT.len();
    let open = item_v2(ITEM_PARENT, &none(), &[EditV2 { span: (end, end), insert_sep: Some("\n\n"), raw: "And then" }]);
    assert!(open.reasons.contains(&"form_paragraph_short_paragraph".to_string()), "{:?}", open.reasons);
    // The echo screen sees every edit: a verbatim copy of the replaced span
    // in the second edit rejects the item; an insertion that adds no token
    // cannot be built (an empty replacement), so it fails assembly too.
    let echo = item_v2(ITEM_PARENT, &none(), &[
        EditV2 { span: (ins, ins), insert_sep: Some(" "), raw: "The roof had leaked for many winters before the work began." },
        EditV2 { span: (rs, re), insert_sep: None, raw: &ITEM_PARENT[rs..re] },
    ]);
    assert!(echo.reasons.contains(&"echo_span_byte_identical".to_string()), "{:?}", echo.reasons);
    assert_eq!(echo.record["echo"]["rejected_edits"], 1);
    // No edit fails closed; overlapping edits fail assembly.
    assert!(item_v2(ITEM_PARENT, &none(), &[]).reasons.contains(&"echo_mixed_no_edits".to_string()));
    let ov = item_v2(ITEM_PARENT, &none(), &[
        EditV2 { span: (rs, re), insert_sep: None, raw: "One new sentence here for the museum." },
        EditV2 { span: (rs, re), insert_sep: None, raw: "Another new sentence for the museum." },
    ]);
    assert!(ov.reasons.contains(&"form_assembly_error:edit order or overlap".to_string()), "{:?}", ov.reasons);
    assert_no_text(&it.record, &[ITEM_PARENT, "The roof had leaked for many winters before the work began."]);
}

#[test]
fn an_insertion_takes_its_point_column_and_region() {
    // Inline in a hard-wrapped parent: the first line starts after the line's
    // body before the point and the separator (53 characters), at W = 67.
    let at = EMAIL.find("quickly.").unwrap() + "quickly.".len();
    let src = SourceLayout::insertion(EMAIL, at, " ").unwrap();
    assert_eq!((src.column, src.width, src.separators.len()), (53, Some(67), 0));
    let raw = "Bob said so too and he asked for the budget item to move up the list.";
    let it = item_v2(EMAIL, &none(), &[EditV2 { span: (at, at), insert_sep: Some(" "), raw }]);
    // Amendment 13 item 13.7: the last line shares its line with the 15
    // columns of parent text after the point, so the greedy last line (55)
    // would end at 70 > 67; its maximal suffix of words that fits with them
    // moves to a new last line.
    assert_eq!(src.tail_column, 15);
    let g = greedy_chars(raw, 67, 53);
    assert_eq!(g.len(), 2);
    assert_eq!(it.reps[0], format!("{}\ntoo and\nhe asked for the budget item to move up the list.", g[0]));
    assert!(it.reps[0].lines().last().unwrap().chars().count() + 15 <= 67);
    assert_eq!(it.record["edits"][0]["transfer"]["tail_budget_splits"], 1);
    // The screen reads the item by construction (item 13.1): the inserted
    // text joins its parent paragraph, so the item is accepted; a
    // re-measurement would split the paragraph at the splice (recorded only).
    assert!(it.reasons.is_empty(), "{:?}", it.reasons);
    assert_eq!(it.record["edits"][0]["remeasure"]["differs"], true);
    // After a newline in the separator, the column restarts.
    assert_eq!(SourceLayout::insertion(EMAIL, EMAIL.len(), "\n\n").unwrap().column, 0);
    // Into a list: the region is line-form and fails the screen, so a list
    // line is not screened; under a prose region it would be.
    let li = LIST.find("- walking boots").unwrap() + "- walking boots".len();
    let lf = item_v2(LIST, &none(), &[EditV2 { span: (li, li), insert_sep: Some("\n"), raw: "- dry socks" }]);
    assert_eq!(lf.record["edits"][0]["form_screen_applied"], false);
    assert!(lf.reasons.is_empty(), "{:?}", lf.reasons);
    assert!(lf.assembled.unwrap().text.contains("- walking boots\n- dry socks\n- a flask"));
    let ra = RAID.find("main hall.").unwrap() + "main hall.".len();
    let pr = item_v2(RAID, &none(), &[EditV2 { span: (ra, ra), insert_sep: Some(" "), raw: "- dry socks" }]);
    assert!(pr.reasons.contains(&"form_line_list_like".to_string()), "{:?}", pr.reasons);
}

#[test]
fn a_single_span_item_is_child_v2() {
    // item_v2 with one replacement agrees with child_v2 on every fixture span.
    for parent in [RAID, EMAIL, MIXED_SEPS, EMAIL_GREETING, WRAPPED_UNICODE] {
        let m = Measure::of(parent);
        for k in 0..m.paragraphs.len() {
            let (s, e) = m.paragraphs[k].range;
            for raw in [CHILD_PROSE, CHILD_LIST] {
                let c = child_v2(parent, Some((s, e)), &none(), raw);
                let it = item_v2(parent, &none(), &[EditV2 { span: (s, e), insert_sep: None, raw }]);
                assert_eq!(it.reps[0], c.text);
                assert_eq!(it.reasons, c.reasons, "{parent:.20} {k}");
                assert_eq!(it.record["edits"][0]["transfer"], c.record["transfer"]);
            }
        }
    }
}

// ------------------------------------------------------------------ donors

#[test]
fn a_donor_lays_out_a_parentless_row() {
    // Review finding 9: the donor's layout only; the screen iff the donor
    // passes it; no echo comparison (no parent).
    let donor = SourceLayout::whole(EMAIL);
    let c = donor_child_v2(&donor, &none(), CHILD_PROSE);
    assert!(c.reasons.is_empty(), "{:?}", c.reasons);
    assert_eq!(c.text, transfer(CHILD_PROSE, &none(), &donor).text);
    assert!(c.text.lines().all(|l| l.chars().count() <= 67));
    assert_eq!((c.record["form_screen_applied"].clone(), c.record["echo"]["applied"].clone()), (Value::Bool(true), Value::Bool(false)));
    assert_eq!(c.record["layout_source"], "donor");
    assert_no_text(&c.record, &[EMAIL, CHILD_PROSE]);
    // A screened donor rejects a list child; a line-form donor does not screen.
    assert!(donor_child_v2(&donor, &none(), CHILD_LIST).reasons.contains(&"form_line_list_like".to_string()));
    let lf = donor_child_v2(&SourceLayout::whole(LIST), &none(), CHILD_LIST);
    assert!(lf.reasons.is_empty() && lf.record["form_screen_applied"] == false);
    // A span is not a donor.
    let sp = SourceLayout::span(EMAIL, 0, 6).unwrap();
    assert_eq!(donor_child_v2(&sp, &none(), CHILD_PROSE).reasons, vec!["form_donor_is_a_span".to_string()]);
}

#[test]
fn each_population_has_its_own_norm_prior_seed() {
    // Amendment 7 item 7.8: V21's plan key with x = "norm_prior".
    let s = norm_prior_seed("train2");
    assert_eq!(s.len(), 64);
    assert_eq!(s, hex::encode(<sha2::Sha256 as sha2::Digest>::digest(b"slop_ninja_a_comp_r2_plan_v1:norm_prior:train2")));
    assert_ne!(s, norm_prior_seed("cal2"));
}
