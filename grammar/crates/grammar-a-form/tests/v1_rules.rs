//! The v1 form-rule tests, run against this crate's paths. They are copies
//! of `grammar-cc-human`'s `era_norm` tests (which stay there unchanged and
//! now exercise the re-exports), plus the line-screen cases of
//! `grammar_a_pools::{fineweb, text}`. Synthetic text only.
use grammar_a_form::conventions::*;
use grammar_a_form::layout::*;
use grammar_a_form::profile::*;
use grammar_a_form::screen::*;
use grammar_a_form::text::{self, GAP, Para, Screen};
use grammar_a_form::window::LENGTH_WINDOW;
use serde_json::json;
use std::collections::{BTreeMap, HashSet};
use std::path::Path;

/// `grammar_a_pools::text::folded_tokens`, which stays in grammar-a-pools.
fn folded_tokens(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    for c in s.chars() {
        if c.is_alphanumeric() {
            cur.extend(c.to_lowercase());
        } else if !cur.is_empty() {
            out.push(std::mem::take(&mut cur));
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

fn conv(d: Option<Quote>, s: Option<Quote>, dash: Option<Dash>, range: Option<Range>, ell: Option<Ellipsis>) -> Conventions {
    let c = |v| Choice { value: v, source: "parent" };
    Conventions { double: c(d), single: c(s), dash: Choice { value: dash, source: "parent" }, range: Choice { value: range, source: "parent" }, ellipsis: Choice { value: ell, source: "parent" } }
}

#[test]
fn min_paragraph_words_matches_the_prose_screen() {
    let seven = "One two three four five six seven.";
    let eight = "One two three four five six seven eight.";
    assert_eq!(classify(seven, false).reason, Some("short_paragraph"));
    assert!(classify(eight, false).prose);
    assert_eq!(text::words(eight), MIN_PARAGRAPH_WORDS);
}

#[test]
fn sites_find_dashes_ranges_and_ellipses() {
    let t = "It rained\u{2014}hard. Then \u{2014} briefly \u{2013} it cleared - mostly -- or so. In 1990-1995 and 3\u{2013}4 days... wait\u{2026} 2010-05-01 COVID-19 well--known";
    let s = sites(t);
    let dashes: Vec<Dash> = s.iter().filter_map(|x| if let Site::Dash(_, _, d) = x { Some(*d) } else { None }).collect();
    assert_eq!(dashes, vec![Dash::EmClosed, Dash::EmSpaced, Dash::EnSpaced, Dash::HyphenSpaced, Dash::DoubleSpaced, Dash::DoubleClosed]);
    let ranges: Vec<Range> = s.iter().filter_map(|x| if let Site::Range(_, _, r) = x { Some(*r) } else { None }).collect();
    assert_eq!(ranges, vec![Range::Hyphen, Range::En]);
    let ell: Vec<Ellipsis> = s.iter().filter_map(|x| if let Site::Ellipsis(_, _, e) = x { Some(*e) } else { None }).collect();
    assert_eq!(ell, vec![Ellipsis::Dots, Ellipsis::Char]);
    // A site's range covers exactly the text it replaces.
    if let Site::Dash(a, b, _) = s[1] {
        assert_eq!(&t[a..b], " \u{2014} ");
    }
    assert!(sites("A \u{2014}\u{2014} B and \u{2014}lead").is_empty());
}

#[test]
fn counts_and_majorities() {
    let c = counts("\u{201C}Hi,\u{201D} she said. It\u{2019}s \"fine\" \u{2014} really\u{2014}truly\u{2014}so.");
    assert_eq!((c.dq_curly, c.dq_ascii, c.sq_curly, c.sq_ascii), (2, 2, 1, 0));
    let o = own(&c);
    assert_eq!(o.double, None);
    assert_eq!(o.single, Some(Quote::Curly));
    assert_eq!(o.quotes, Some(Quote::Curly));
    assert_eq!(o.dash, Some(Dash::EmClosed));
    assert_eq!(o.range, None);
}

#[test]
fn resolution_falls_back_to_the_other_quote_then_a_keyed_draw() {
    let prior = Prior {
        parents: 10,
        quotes: BTreeMap::from([(Quote::Curly, 6), (Quote::Ascii, 4)]),
        dash: BTreeMap::from([(Dash::EmClosed, 1)]),
        range: BTreeMap::new(),
        ellipsis: BTreeMap::from([(Ellipsis::Dots, 3)]),
    };
    let c = counts("It\u{2019}s here and it\u{2019}s fine.");
    let r = resolve(&c, &prior, "s", "doc-a");
    assert_eq!(r.single, Choice { value: Some(Quote::Curly), source: "parent" });
    assert_eq!(r.double, Choice { value: Some(Quote::Curly), source: "parent_other_quote" });
    assert_eq!(r.dash, Choice { value: Some(Dash::EmClosed), source: "keyed_prior" });
    assert_eq!(r.range, Choice { value: None, source: "none" });
    assert_eq!(r.ellipsis.value, Some(Ellipsis::Dots));
    // No quotes at all: both quote classes take the same keyed draw, stable per document.
    let none = counts("Plain words only here.");
    let mut seen = HashSet::new();
    for k in 0..64 {
        let a = resolve(&none, &prior, "s", &format!("doc-{k}"));
        assert_eq!(a.double.value, a.single.value);
        assert_eq!(a.double.source, "keyed_prior");
        assert_eq!(a, resolve(&none, &prior, "s", &format!("doc-{k}")));
        seen.insert(a.double.value);
    }
    assert_eq!(seen.len(), 2, "both conventions are drawn");
}

#[test]
fn quotes_render_by_context() {
    let mut ch = Changes::default();
    let c = conv(Some(Quote::Curly), Some(Quote::Curly), None, None, None);
    assert_eq!(render_quotes("\"Don't,\" she said, 'twas the '90s and 'quiet' rock 'n' roll.", &c, &mut ch),
        "\u{201C}Don\u{2019}t,\u{201D} she said, \u{2019}twas the \u{2019}90s and \u{2018}quiet\u{2019} rock \u{2019}n\u{2019} roll.");
    assert_eq!(ch.double_quotes, 2);
    let a = conv(Some(Quote::Ascii), Some(Quote::Ascii), None, None, None);
    assert_eq!(render_quotes("\u{201C}It\u{2019}s\u{201D}", &a, &mut ch), "\"It's\"");
}

#[test]
fn dashes_ranges_and_ellipses_take_the_parent_style() {
    let mut ch = Changes::default();
    let c = conv(None, None, Some(Dash::HyphenSpaced), Some(Range::En), Some(Ellipsis::Char));
    assert_eq!(render_sites("It rained\u{2014}hard, from 1990-1995... then -- stopped.", &c, &mut ch), "It rained - hard, from 1990\u{2013}1995\u{2026} then - stopped.");
    assert_eq!((ch.dashes, ch.ranges, ch.ellipses), (2, 1, 1));
    let c2 = conv(None, None, Some(Dash::EmClosed), None, None);
    assert_eq!(render_sites("a - b \u{2013} c", &c2, &mut ch), "a\u{2014}b\u{2014}c");
}

#[test]
fn overlapping_sites_skip_the_later_one() {
    // Two spaced hyphens one space apart share that space; rendering both
    // panicked. The later is left as written, as gen_form_inject_v1 does.
    let t = "It was - - odd.";
    assert_eq!(sites(t), vec![Site::Dash(6, 9, Dash::HyphenSpaced), Site::Dash(8, 11, Dash::HyphenSpaced)]);
    let em = conv(None, None, Some(Dash::EmClosed), None, None);
    let mut ch = Changes::default();
    assert_eq!(render_sites(t, &em, &mut ch), "It was\u{2014}- odd.");
    assert_eq!(ch.dashes, 1);
    let mut ch = Changes::default();
    assert_eq!(render_sites(t, &conv(None, None, Some(Dash::EmSpaced), None, None), &mut ch), "It was \u{2014} - odd.");
    assert_eq!(ch.dashes, 1);
    // nothing to replace: unchanged
    let mut ch = Changes::default();
    assert_eq!(render_sites(t, &conv(None, None, Some(Dash::HyphenSpaced), None, None), &mut ch), t);
    assert_eq!(ch, Changes::default());
    // a third site is compared with the last replaced one, not the skipped one
    let mut ch = Changes::default();
    assert_eq!(render_sites("a - - - b", &em, &mut ch), "a\u{2014}-\u{2014}b");
    assert_eq!(ch.dashes, 2);
    // a site that keeps its style is not replaced, so the overlapping one after it is
    let mixed = "a \u{2014} - b";
    assert_eq!(sites(mixed), vec![Site::Dash(1, 6, Dash::EmSpaced), Site::Dash(5, 8, Dash::HyphenSpaced)]);
    let mut ch = Changes::default();
    assert_eq!(render_sites(mixed, &conv(None, None, Some(Dash::EmSpaced), None, None), &mut ch), "a \u{2014} \u{2014} b");
    assert_eq!(render_sites(mixed, &conv(None, None, Some(Dash::EnSpaced), None, None), &mut ch), "a \u{2013} - b");
    assert_eq!(ch.dashes, 2);
    // touching sites are both rendered
    let mut ch = Changes::default();
    let c = conv(None, None, Some(Dash::EmSpaced), None, Some(Ellipsis::Char));
    assert_eq!(render_sites("a\u{2014}... b", &c, &mut ch), "a \u{2014} \u{2026} b");
    assert_eq!((ch.dashes, ch.ellipses), (1, 1));
}

#[test]
fn layout_drops_trailing_space_and_single_breaks() {
    let mut ch = Changes::default();
    let lines = layout_lines("First line here.  \n  Second\tline.\n\n\nThird line.", &mut ch);
    assert_eq!(lines, vec!["First line here.", "Second line.", "Third line."]);
    assert_eq!(ch.trailing_space_lines, 1);
    assert_eq!(ch.leading_space_lines, 1);
    assert_eq!(ch.single_breaks, 1);
    assert_eq!(ch.blank_lines_dropped, 1);
    assert_eq!(excerpt_layout("a b.\n\nc  d. \n e."), "a b.\n\nc d.\n\ne.");
    let mut ch2 = Changes::default();
    assert_eq!(strip_emphasis("A **bold** and __under__ word **x", &mut ch2), "A bold and under word **x");
    assert_eq!(ch2.markdown_emphasis_pairs, 2);
}

#[test]
fn breaks_follow_the_parent_fractions_with_a_paragraph_floor() {
    // Ten 5-word sentences; parent paragraphs at 30% and 70%.
    let words = vec![5; 10];
    let allowed = vec![true; 11];
    let st = Structure { fractions: vec![0.3, 0.7], pre_words: 0, post_words: 0 };
    assert_eq!(place_breaks(&words, &allowed, &st), (vec![3, 7], 0));
    // A 5% paragraph cannot be kept at 8 words when sentences have 5: it moves.
    let st2 = Structure { fractions: vec![0.05], pre_words: 0, post_words: 0 };
    assert_eq!(place_breaks(&words, &allowed, &st2), (vec![2], 0));
    // Too few words for both breaks: the smallest parent pair is merged.
    let st3 = Structure { fractions: vec![0.3, 0.5], pre_words: 0, post_words: 0 };
    assert_eq!(place_breaks(&[5, 5, 5, 5], &[true; 5], &st3), (vec![2], 1));
    assert_eq!(place_breaks(&[5, 5, 5], &[true; 4], &st3), (vec![], 2));
    // Joined parent words count toward the first and last paragraphs.
    let st4 = Structure { fractions: vec![0.1], pre_words: 20, post_words: 0 };
    assert_eq!(place_breaks(&words, &allowed, &st4), (vec![1], 0));
    // Disallowed boundaries are never used.
    let mut al = vec![true; 11];
    al[3] = false;
    assert_eq!(place_breaks(&words, &al, &st).0, vec![2, 7]);
}

#[test]
fn break_ties_go_to_the_earliest_boundaries() {
    // Exact binary fractions, so the tied costs are equal in f64. The golden
    // pins the same grids (breaks/12..14) from the v1 code.
    let st = |f: Vec<f64>| Structure { fractions: f, pre_words: 0, post_words: 0 };
    // Final choice: breaks after 16 or 48 of 64 words are both 0.25 from 0.5.
    assert_eq!(place_breaks(&[16, 32, 16], &[true; 4], &st(vec![0.5])), (vec![1], 0));
    // Inner step: breaks after 1 or 2 of 8 sentences are both 0.0625 from
    // 0.1875; the second break lands on 0.75 either way.
    assert_eq!(place_breaks(&[8; 8], &[true; 9], &st(vec![0.1875, 0.75])), (vec![1, 6], 0));
    // Drop: 28 words cannot hold four 8-word paragraphs, and every target's
    // two parent paragraphs sum to 0.5, so the first target is dropped.
    assert_eq!(place_breaks(&[4; 7], &[true; 8], &st(vec![0.25, 0.5, 0.75])), (vec![3, 5], 1));
}

#[test]
fn child_takes_the_parent_form_and_the_screen_rejects_lists_and_titles() {
    let parent = "\u{201C}We left at dawn,\u{201D} the captain\u{2019}s mate said to the crew that morning.\n\nThe harbour was quiet and the gulls followed the ferry for a while before turning back to land.";
    let c = counts(parent);
    let prior = Prior::of(std::iter::once(parent));
    let conv = resolve(&c, &prior, "s", "d");
    let child = "The ferry's horn sounded twice as it left the pier.  \nNobody on the deck said a word about the \"storm\" coming in from the west.\nThe passengers waited below. They talked about the weather and their plans for the evening ahead.";
    let n = normalise_child(child, &conv, &Structure::whole(parent));
    assert!(n.line_reasons.is_empty());
    assert_eq!(n.layout.realised_breaks, 1);
    assert!(!n.text.contains("  ") && !n.text.contains(" \n") && n.text.matches("\n\n").count() == 1);
    assert!(n.text.contains("ferry\u{2019}s") && n.text.contains("\u{201C}storm\u{201D}"));
    assert_eq!(n.changes.trailing_space_lines, 1);
    assert!(paragraph_screen(&n.text, None).is_empty());
    // Folded tokens are unchanged.
    assert_eq!(folded_tokens(child), folded_tokens(&n.text));
    let titled = normalise_child("A Morning at Sea\nThe ferry left the harbour at dawn and the gulls followed it out.", &conv, &Structure::whole(parent));
    assert_eq!(titled.line_reasons, vec!["form_line_no_sentence_end"]);
    let listed = normalise_child("1. Pack the bags before you leave for the harbour.\n2. Check the ferry times twice before leaving.", &conv, &Structure::whole(parent));
    assert!(listed.line_reasons.contains(&"form_line_list_like".to_string()));
    let quoted = "The mate said \u{201C}we will leave the harbour at dawn whatever the weather does\u{201D} and walked away.";
    assert_eq!(paragraph_screen(quoted, None), vec!["form_paragraph_long_quotation"]);
}

#[test]
fn a_consistent_excerpt_is_a_fixed_point_of_its_own_profile() {
    let human = "The council met on Tuesday \u{2014} late, as usual \u{2014} to discuss the 2015\u{2013}2016 budget.\n\nMembers said the plan\u{2019}s costs were too high, and one called it \u{201C}a waste\u{201D}\u{2026} and left.\n\nA vote is expected next week, after the public hearing on the matter.";
    let c = counts(human);
    let conv = resolve(&c, &Prior::default(), "s", "h");
    let n = normalise_child(human, &conv, &Structure::whole(human));
    assert_eq!(n.text, human);
    assert_eq!(n.changes, Changes::default());
    assert_eq!(every_class(human, Profile::NormV1).0, human);
}

#[test]
fn profiles_and_the_every_class_length_window() {
    for p in [Profile::Raw, Profile::NormV1, Profile::CohortV1] {
        assert_eq!(Profile::parse(p.id()).unwrap(), p);
    }
    assert!(Profile::parse("layout_v0").is_err());
    assert_eq!(Profile::DEFAULT, Profile::CohortV1);
    assert_eq!(Profile::CohortV1.window(), Some((150, 450)));
    assert_eq!(Profile::NormV1.window(), None);
    assert!(Profile::CohortV1.check_prior(None).is_err());
    assert!(Profile::Raw.check_prior(Some(Path::new("x"))).is_err());
    assert!(Profile::Raw.check_prior(None).is_ok());
    // The window is the human excerpt bound.
    assert_eq!(LENGTH_WINDOW, (text::MIN_WORDS, text::MAX_WORDS));
    let para = "The ferry left the harbour at dawn and the gulls followed it out to sea for a while.";
    let doc = |n: usize| vec![para; n].join("\n\n");
    // 18 words per paragraph: 8 paragraphs = 144 words (short), 9 = 162, 25 = 450 (inclusive), 26 = 468 (long).
    for (n, ok) in [(8usize, false), (9, true), (25, true), (26, false)] {
        let (_, rec, why) = every_class(&doc(n), Profile::CohortV1);
        assert_eq!(why.is_empty(), ok, "{n}: {why:?}");
        assert_eq!(rec["excluded"], json!(!ok));
        assert_eq!(rec["length_window"]["pass"], json!(ok));
        // The format-v1 profile records the same document but excludes nothing.
        let (_, r1, w1) = every_class(&doc(n), Profile::NormV1);
        assert!(w1.is_empty() && r1["excluded"] == json!(false) && r1.get("length_window").is_none());
    }
    // A paragraph failing the screen fails every class alike.
    let bad = format!("{}\n\nRead more at www.example.com today", doc(9));
    let (_, _, why) = every_class(&bad, Profile::CohortV1);
    assert!(why.iter().any(|r| r.starts_with("form_paragraph_")), "{why:?}");
}

#[test]
fn mixed_span_structure_and_breaks_at_sentence_starts_only() {
    let parent = "Alpha one two three four five six. Beta one two three four five six.\n\nGamma one two three four five six. Delta one two three four five.";
    let s = parent.find("Beta").unwrap();
    let e = parent.find("Delta").unwrap() - 1;
    let st = Structure::span(parent, s, e);
    assert_eq!(st.pre_words, 7);
    assert_eq!(st.post_words, 6);
    assert_eq!(st.fractions.len(), 1);
    assert!(ends_with_abbreviation("He met Dr.") && ends_with_abbreviation("by J.") && !ends_with_abbreviation("He left."));
    assert!(starts_sentence("\u{201C}Yes") && !starts_sentence("and then"));
}

// ------------------------------------------------------------------ pool line screens

#[test]
fn line_screens() {
    assert!(list_like("- an item that ends with a period."));
    assert!(list_like("12. Twelfth item in the list goes here."));
    assert!(!list_like("In 2012. the year it all changed, we moved."));
    assert!(boilerplate("Please read our Privacy Policy before you continue with this site."));
    assert!(boilerplate("Write to me at someone@example.com if you have any questions."));
    let p = classify("This is an ordinary sentence of running prose written by the site owner.", false);
    assert!(p.prose);
    let r = classify("This is an ordinary sentence of running prose written by the site owner.", true);
    assert_eq!(r.reason, Some("repeated_on_site_template"));
}

fn p(t: &str) -> Para {
    Para::classify(t, Screen::MARKUP)
}

fn sentence(n: usize) -> String {
    let mut s = vec!["word"; n].join(" ");
    s.push('.');
    s
}

#[test]
fn prose_screen_rejects_gaps_markup_and_quotes() {
    assert!(p(&sentence(20)).prose);
    assert_eq!(p("A short one.").reason, Some("short_paragraph"));
    assert_eq!(p(&format!("{} no end", sentence(10))).reason, Some("no_sentence_end"));
    assert_eq!(p(&format!("See {} [[x]].", sentence(9))).reason, Some("residual_markup"));
    assert_eq!(p(&format!("{GAP} {}", sentence(10))).reason, Some("removed_inline_content"));
    let q = format!("{} He said \"one two three four five six seven eight\".", sentence(5));
    assert_eq!(p(&q).reason, Some("long_quotation"));
    assert!(Para::classify(q, Screen { quotes: false, angle_brackets: true }).prose);
    assert!(Para::classify(format!("{} p < 0.05.", sentence(9)), Screen::XML).prose);
    assert_eq!(p(&format!("{} https://example.org.", sentence(9))).reason, Some("url"));
}

#[test]
fn profile_errors_keep_the_v1_messages() {
    assert_eq!(Profile::parse("layout_v0").unwrap_err().to_string(),
        "unknown cohort profile layout_v0 (known: cc_cohort_profile_v1, cc_era_parent_norm_v1, raw)");
    assert_eq!(Profile::CohortV1.check_prior(None).unwrap_err().to_string(), "profile cc_cohort_profile_v1 needs the normaliser prior (--norm)");
    assert_eq!(Profile::Raw.check_prior(Some(Path::new("x"))).unwrap_err().to_string(), "profile raw takes no normaliser prior");
    let boxed: Box<dyn std::error::Error + Send + Sync> = Box::new(ProfileError::Unknown("x".into()));
    assert!(boxed.to_string().starts_with("unknown cohort profile x"));
}
