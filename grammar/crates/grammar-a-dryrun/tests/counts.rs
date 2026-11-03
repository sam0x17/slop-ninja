//! B23's counting kernel on synthetic text only (no corpus text, no live
//! call): each stop invariant's counted case and its near misses, counts 1-3
//! on amendment 7's code against the landed code, the re-derivation
//! identity and the merge.
use grammar_a_dryrun::engine::{self, Code, Failure, Source};
use grammar_a_dryrun::invariants::{Final, invariants};
use grammar_a_dryrun::tally::{self, Case, Fin, Recorded, Tally};
use grammar_a_dryrun::{Split, line_split, sha_hex};
use grammar_a_form::conventions::{Choice, Conventions, Dash};
use grammar_a_form::layout::ParagraphSource;
use grammar_a_form::transfer::{KeptKind, KeptUnit, SourceLayout, TransferDiag, Transferred, UnitDiag, UnitRole, transfer};
use serde_json::{Value, json};

const SYN: [&str; 20] = [
    "amber", "basalt", "cobalt", "delta", "ember", "fjord", "garnet", "harbor", "indigo", "juniper", "kestrel", "lantern", "meadow", "nectar",
    "orchid", "pebble", "quartz", "river", "saffron", "timber",
];
const ALT: [&str; 20] = [
    "acorn", "bramble", "canyon", "dune", "estuary", "fern", "glacier", "heather", "inlet", "jasper", "knoll", "lichen", "marsh", "nettle",
    "oasis", "prairie", "quarry", "ridge", "sorrel", "thicket",
];

/// `n` sentences of `len` words from `vocab`, each capitalised and ended
/// with a period (`end = false` leaves the last unpunctuated).
fn prose(vocab: &[&str], seed: u64, n: usize, len: usize, end: bool) -> String {
    let mut st = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
    let mut out: Vec<String> = Vec::new();
    for k in 0..n {
        let ws: Vec<&str> = (0..len)
            .map(|_| {
                st = st.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                vocab[((st >> 33) as usize) % vocab.len()]
            })
            .collect();
        let s = ws.join(" ");
        let p = if end || k + 1 < n { "." } else { "" };
        out.push(format!("{}{}{p}", s[..1].to_uppercase(), &s[1..]));
    }
    out.join(" ")
}

fn none_json() -> Value {
    json!({"double": {"value": null, "source": "none"}, "single": {"value": null, "source": "none"}, "dash": {"value": null, "source": "none"},
        "range": {"value": null, "source": "none"}, "ellipsis": {"value": null, "source": "none"}})
}

fn none() -> Conventions {
    engine::landed_conventions(&none_json()).unwrap()
}

// ------------------------------------------------------------------ engine

#[test]
fn recorded_conventions_are_read_and_must_round_trip() {
    let c = none();
    assert_eq!(serde_json::to_value(&c).unwrap(), none_json());
    let mut v = none_json();
    v["dash"] = json!({"value": "em_closed", "source": "keyed_prior_domain"});
    let c = engine::landed_conventions(&v).unwrap();
    assert_eq!(c.dash, Choice { value: Some(Dash::EmClosed), source: "keyed_prior_domain" });
    assert!(engine::a7_conventions(&v).is_ok());
    let mut bad = none_json();
    bad["dash"]["source"] = json!("an invented source");
    assert_eq!(engine::landed_conventions(&bad).unwrap_err(), Failure::Conventions("unknown source"));
    let mut extra = none_json();
    extra["dash"]["note"] = json!(1);
    assert_eq!(engine::landed_conventions(&extra).unwrap_err(), Failure::Conventions("does not round-trip"));
}

#[test]
fn amendment_7s_panic_on_overlapping_dash_sites_is_caught_and_the_landed_code_runs() {
    engine::quiet_panics();
    let mut conv = none_json();
    conv["dash"] = json!({"value": "em_closed", "source": "parent"});
    let parent = format!("{}\n\n{}", prose(&SYN, 1, 4, 10, true), prose(&SYN, 2, 4, 10, true));
    let raw = format!("{} The keeper wrote amber - - harbor twice. {}", prose(&ALT, 3, 3, 10, true), prose(&ALT, 4, 3, 10, true));
    let a7 = engine::child_v2(Code::A7, &parent, None, &conv, &raw);
    assert!(matches!(&a7, Err(Failure::Panic(at)) if at.contains("conventions.rs")), "{a7:?}");
    assert!(a7.unwrap_err().code().starts_with("panic:conventions.rs"));
    assert!(engine::child_v2(Code::Landed, &parent, None, &conv, &raw).is_ok());
}

#[test]
fn split_keys_are_read_from_the_bytes() {
    assert_eq!(line_split(br#"{"split":"train","x":1}"#, "split"), Split::One("train".into()));
    assert_eq!(line_split(br#"{"split" : "dev_select", "split":"dev_select"}"#, "split"), Split::One("dev_select".into()));
    assert_eq!(line_split(br#"{"x":"split"}"#, "split"), Split::Missing);
    assert_eq!(line_split(br#"{"split":"train","y":{"split":"test"}}"#, "split"), Split::Unreadable);
    assert_eq!(line_split(br#"{"split":"tr\"ain"}"#, "split"), Split::Unreadable);
    assert_eq!(line_split(b"{\"split\":\"dev_select\",\x00 broken", "split"), Split::One("dev_select".into()));
}

// ------------------------------------------------------------------ count 4

/// A transferred child with the given units (text, role, wrapped,
/// segment), joined by blank lines, and a source of width `w`.
fn hand(units: &[(&str, UnitRole, bool, Option<usize>)], w: Option<usize>, mixed: bool) -> (SourceLayout, Transferred) {
    let parent = format!("{}\n\n{}", prose(&SYN, 5, 3, 9, true), prose(&SYN, 6, 3, 9, true));
    let mut src = SourceLayout::whole(&parent);
    src.width = w;
    src.mixed = mixed;
    let mut t = transfer(&prose(&ALT, 7, 2, 9, true), &none(), &src);
    let mut text = String::new();
    let mut paragraphs = Vec::new();
    let mut diag = Vec::new();
    for (k, (u, role, wrapped, segment)) in units.iter().enumerate() {
        if k > 0 {
            text.push_str("\n\n");
        }
        let a = text.len();
        text.push_str(u);
        paragraphs.push((a, text.len()));
        diag.push(UnitDiag { role: *role, wrapped: *wrapped, segment: *segment, source: ParagraphSource { raw: (k * 100, k * 100 + 50), starts_mid_line: false, ends_mid_line: false } });
    }
    t.text = text;
    t.paragraphs = paragraphs;
    t.diag = TransferDiag { units: diag, kept: Vec::new() };
    (src, t)
}

const B: UnitRole = UnitRole::Body;

#[test]
fn count4_a_wrapped_two_word_line_over_its_budget_is_counted_and_a_one_word_line_is_not() {
    let (src, t) = hand(&[("alpha beta gamma\ndeltaepsilonzetaetathetaiota\nkappa lambda", B, true, Some(0))], Some(20), false);
    assert_eq!(invariants(&src, &t, None).over_budget.len(), 0, "a one-word line wider than W is exempt");
    let (src, t) = hand(&[("alpha beta gamma\ndeltaepsilon zetaetathetaiota\nkappa lambda", B, true, Some(0))], Some(20), false);
    let inv = invariants(&src, &t, None);
    assert_eq!((inv.wrapped_lines, inv.over_budget.len()), (3, 1));
    assert_eq!((inv.over_budget[0].unit, inv.over_budget[0].line), (0, 1));
    // near miss: exactly W columns, lead included, is within the budget
    let (src, t) = hand(&[("alpha beta gamma\n  deltaepsilon zetas\nkappa lambda", B, true, Some(0))], Some(20), false);
    assert_eq!(invariants(&src, &t, None).over_budget.len(), 0);
    let (src, t) = hand(&[("alpha beta gamma\n   deltaepsilon zetas\nkappa lambda", B, true, Some(0))], Some(20), false);
    assert_eq!(invariants(&src, &t, None).over_budget.len(), 1, "the lead counts in the columns");
    // trailing whitespace does not count
    let (src, t) = hand(&[("alpha beta gamma delta     \nkappa", B, true, Some(0))], Some(22), false);
    assert_eq!(invariants(&src, &t, None).over_budget.len(), 0);
    // diagnostics that do not cover every unit are inconsistent
    let (src, mut t) = hand(&[("alpha beta", B, true, Some(0)), ("gamma delta", B, true, Some(0))], Some(20), false);
    t.diag.units.pop();
    assert!(invariants(&src, &t, None).inconsistent.is_some());
    // an unwrapped paragraph and a kept line have no budget
    let (src, t) = hand(&[("alpha beta gamma delta epsilon zeta eta theta", B, false, Some(0)), ("iota kappa lambda mu nu xi omicron", UnitRole::KeptLine, false, None)], Some(10), false);
    assert_eq!(invariants(&src, &t, None).over_budget.len(), 0);
}

#[test]
fn count4_a_mixed_childs_last_line_that_fits_alone_but_not_with_the_parent_text_after_it_is_counted() {
    let mk = |tail: usize| {
        let (mut src, t) = hand(&[("alpha beta\nccc ddd eee", B, true, Some(0))], Some(20), true);
        src.tail_column = tail;
        src.column = 0;
        (src, t)
    };
    let (src, t) = mk(10);
    let inv = invariants(&src, &t, None);
    assert_eq!((inv.tail_lines, inv.over_budget.len()), (1, 1));
    assert!(inv.over_budget[0].with_tail);
    let (src, t) = mk(9);
    assert_eq!(invariants(&src, &t, None).over_budget.len(), 0, "near miss: 11 + 9 = W");
    // a one-word last line with the parent text after it is exempt
    let (mut src, t) = hand(&[("alpha beta\nccc", B, true, Some(0))], Some(20), true);
    src.tail_column = 30;
    assert_eq!(invariants(&src, &t, None).over_budget.len(), 0);
    // a mixed child's first line starts at the source's column
    let (mut src, t) = hand(&[("aaa bbb\nccc", B, true, Some(0))], Some(20), true);
    src.column = 14;
    assert_eq!(invariants(&src, &t, None).over_budget.len(), 1);
    src.column = 13;
    assert_eq!(invariants(&src, &t, None).over_budget.len(), 0);
}

#[test]
fn count4_a_mixed_childs_column_and_tail_column_apply_to_its_first_and_last_lines_only() {
    // two wrapped units: only unit 0's first line starts at the column, only the last unit's last line
    // shares its line with the parent text after the span (each line below is 19 columns or 7)
    let (mut src, t) = hand(&[("aaa bbb\nccc ddd eee fff ggg", B, true, Some(0)), ("hhh iii jjj kkk lll\nmmm nnn", B, true, Some(0))], Some(20), true);
    src.column = 12;
    src.tail_column = 12;
    let inv = invariants(&src, &t, None);
    assert_eq!((inv.wrapped_lines, inv.tail_lines, inv.over_budget.len()), (4, 1, 0), "{inv:?}");
    // near misses: one more column on either edge puts that line over
    src.column = 14;
    let inv = invariants(&src, &t, None);
    assert_eq!(inv.over_budget.iter().map(|x| (x.unit, x.line)).collect::<Vec<_>>(), vec![(0, 0)]);
    src.column = 12;
    src.tail_column = 14;
    let inv = invariants(&src, &t, None);
    assert_eq!(inv.over_budget.iter().map(|x| (x.unit, x.line, x.with_tail)).collect::<Vec<_>>(), vec![(1, 1, true)]);
}

#[test]
fn count4_the_document_reading_measures_the_whole_document_line() {
    let (src, t) = hand(&[("alpha beta\nccc ddd", B, true, Some(0))], Some(20), true);
    // the child sits on a line whose parent text after it (another edit's) makes it wider than W
    let doc = format!("Lead words {}  and a long tail of words", t.text);
    let fin = Final { doc: &doc, fin: (11, 11 + t.text.len()) };
    let inv = invariants(&src, &t, Some(fin));
    assert_eq!(inv.over_budget.len(), 0);
    assert_eq!(inv.over_budget_document.len(), 2, "both lines share a document line with parent text");
    // a document that does not hold the child at its range is inconsistent
    assert!(invariants(&src, &t, Some(Final { doc: "x", fin: (0, 1) })).inconsistent.is_some());
}

#[test]
fn count4_real_transfers_obey_their_budgets() {
    // a hard-wrapped parent at W 40; its mirror and span children are wrapped within budget
    let mut lines = Vec::new();
    for k in 0..4 {
        let p = prose(&SYN, 20 + k, 4, 9, true);
        let mut cur = String::new();
        for w in p.split(' ') {
            if !cur.is_empty() && cur.chars().count() + 1 + w.chars().count() > 40 {
                lines.push(std::mem::take(&mut cur));
            }
            if !cur.is_empty() {
                cur.push(' ');
            }
            cur.push_str(w);
        }
        lines.push(cur);
        lines.push(String::new());
    }
    let parent = lines.join("\n");
    let src = SourceLayout::whole(&parent);
    assert!(src.width.is_some(), "the synthetic parent is hard-wrapped");
    let t = transfer(&prose(&ALT, 30, 12, 9, true), &none(), &src);
    let inv = invariants(&src, &t, None);
    assert!(inv.wrapped_lines > 4);
    assert!(inv.over_budget.is_empty() && inv.inconsistent.is_none(), "{inv:?}");
}

// ------------------------------------------------------------------ count 5

/// A prose parent licensing `no_sentence_end`: a punctuated paragraph of
/// `a` words, then an unpunctuated one of `b` words.
fn closing_parent(a: usize, b: usize) -> String {
    format!("{}\n\n{}", prose(&SYN, 40, 1, a, true), prose(&SYN, 41, 1, b, false))
}

#[test]
fn count5_a_closed_unit_joined_into_a_paragraph_is_counted() {
    // the break target (a quarter of the way) falls before the second
    // paragraph, so the closed third paragraph shares its unit with it
    let parent = closing_parent(10, 30);
    let src = SourceLayout::whole(&parent);
    assert!(src.licence.iter().any(|r| r == "no_sentence_end") && !src.line_form, "{:?}", src.licence);
    let raw = format!("{}\n\n{}\n\n{}", prose(&ALT, 42, 1, 10, true), prose(&ALT, 43, 1, 10, true), prose(&ALT, 44, 1, 10, false));
    let t = transfer(&raw, &none(), &src);
    assert_eq!(t.record.closed_paragraphs, 1);
    let inv = invariants(&src, &t, None);
    assert_eq!((inv.closed_units, inv.kept_shared.len()), (1, 1), "{:?}", t.diag);
    assert_eq!(t.diag.kept[inv.kept_shared[0]].kind, KeptKind::Closed);
}

#[test]
fn count5_a_closed_unit_kept_as_its_own_unit_is_not() {
    let parent = closing_parent(10, 10);
    let src = SourceLayout::whole(&parent);
    let raw = format!("{}\n\n{}", prose(&ALT, 45, 1, 10, true), prose(&ALT, 46, 1, 10, false));
    let t = transfer(&raw, &none(), &src);
    assert_eq!(t.record.closed_paragraphs, 1);
    let inv = invariants(&src, &t, None);
    assert_eq!((inv.closed_units, inv.kept_shared.len()), (1, 0), "{:?}", t.diag);
}

#[test]
fn count5_a_kept_list_line_is_its_own_unit_and_one_joined_into_prose_is_counted() {
    let parent = format!("{}\n- {}\n- {}\n\n{}", prose(&SYN, 50, 2, 9, true), prose(&SYN, 51, 1, 6, true), prose(&SYN, 52, 1, 6, true), prose(&SYN, 53, 3, 9, true));
    let src = SourceLayout::whole(&parent);
    assert!(src.licence.iter().any(|r| r == "list_marker"), "{:?}", src.licence);
    let raw = format!("{}\n- {}\n- {}\n\n{}", prose(&ALT, 54, 2, 9, true), prose(&ALT, 55, 1, 6, true), prose(&ALT, 56, 1, 6, true), prose(&ALT, 57, 3, 9, true));
    let mut t = transfer(&raw, &none(), &src);
    assert!(t.record.kept_list_lines >= 2);
    let inv = invariants(&src, &t, None);
    assert!(inv.kept_list_lines >= 2 && inv.kept_shared.is_empty(), "{:?}", t.diag);
    // the same child with one kept line drawn into the paragraph before it
    let k = t.diag.kept.iter().position(|k| k.kind == KeptKind::ListLine).unwrap();
    let kr = t.diag.kept[k].raw;
    let u = t.diag.units.iter().position(|u| u.source.raw == kr).unwrap();
    t.diag.units[u - 1].source.raw.1 = kr.1;
    let inv = invariants(&src, &t, None);
    assert_eq!(inv.kept_shared, vec![k]);
    // a kept unit that no unit draws on is inconsistent
    t.diag.kept.push(KeptUnit { kind: KeptKind::Closed, raw: (raw.len() + 5, raw.len() + 9) });
    assert!(invariants(&src, &t, None).inconsistent.is_some());
}

// ------------------------------------------------------------------ count 6

#[test]
fn count6_a_placed_break_paragraph_of_7_words_is_counted_and_one_of_8_is_not() {
    let seven = "one two three four five six seven.";
    let eight = "one two three four five six seven eight.";
    let long = "alpha beta gamma delta epsilon zeta eta theta iota kappa.";
    let (src, t) = hand(&[(seven, B, false, Some(0)), (long, B, false, Some(0))], None, false);
    let inv = invariants(&src, &t, None);
    assert_eq!((inv.placed_paragraphs, inv.short_alone.clone(), inv.short_final.clone()), (2, vec![0], vec![0]));
    let (src, t) = hand(&[(eight, B, false, Some(0)), (long, B, false, Some(0))], None, false);
    assert_eq!(invariants(&src, &t, None).short_alone.len(), 0);
}

#[test]
fn count6_short_paragraphs_at_fixed_boundaries_are_not_counted() {
    let seven = "one two three four five six seven.";
    let long = "alpha beta gamma delta epsilon zeta eta theta iota kappa.";
    // a short paragraph ending at a fixed boundary: the next unit is another segment
    let (src, t) = hand(&[(seven, B, false, Some(0)), (long, B, false, Some(1))], None, false);
    let inv = invariants(&src, &t, None);
    assert_eq!((inv.placed_paragraphs, inv.short_alone.len()), (0, 0));
    // at a kept unit's edge
    let (src, t) = hand(&[(long, UnitRole::KeptLine, false, None), (seven, B, false, Some(0)), ("- a kept line", UnitRole::KeptLine, false, None)], None, false);
    assert_eq!(invariants(&src, &t, None).short_alone.len(), 0);
    // next to a frame line
    let (src, t) = hand(&[("Hi,", UnitRole::HeadFrame, false, None), (seven, B, false, Some(0)), (long, B, false, Some(1))], None, false);
    assert_eq!(invariants(&src, &t, None).short_alone.len(), 0);
}

#[test]
fn count6_a_spans_first_paragraph_is_read_alone_and_with_the_parent_words_it_joins() {
    let three = "one two three.";
    let long = "alpha beta gamma delta epsilon zeta eta theta iota kappa.";
    let (mut src, t) = hand(&[(three, B, false, Some(0)), (long, B, false, Some(0))], None, true);
    src.structure.pre_words = 6;
    let inv = invariants(&src, &t, None);
    assert_eq!((inv.short_alone.clone(), inv.short_final.clone()), (vec![0], vec![]), "3 alone, 9 with the parent words");
    src.structure.pre_words = 4;
    assert_eq!(invariants(&src, &t, None).short_final, vec![0]);
}

#[test]
fn count6_real_placements_respect_the_floor_with_the_parent_words() {
    // a span starting six words into a parent paragraph, whose first break target is near its start
    let p1 = format!("{} {}", prose(&SYN, 60, 1, 6, true), prose(&SYN, 61, 1, 4, true));
    let parent = format!("{p1}\n\n{}\n\n{}", prose(&SYN, 62, 3, 10, true), prose(&SYN, 63, 2, 10, true));
    let s = parent.find(&prose(&SYN, 61, 1, 4, true)).unwrap();
    let e = parent.len();
    let src = SourceLayout::span(&parent, s, e).unwrap();
    assert_eq!(src.structure.pre_words, 6);
    let raw = format!("{} {} {}", prose(&ALT, 64, 1, 3, true), prose(&ALT, 65, 4, 10, true), prose(&ALT, 66, 2, 10, true));
    let t = transfer(&raw, &none(), &src);
    let inv = invariants(&src, &t, None);
    assert!(inv.placed_paragraphs >= 2, "{:?}", t.diag);
    assert!(inv.short_final.is_empty(), "placement counts the parent words: {inv:?}");
    assert_eq!(inv.short_alone, vec![0], "the first paragraph alone is under 8 words: the two readings differ");
}

// ------------------------------------------------------------------ diagnostics

#[test]
fn transfer_diagnostics_cover_every_unit_and_tell_placed_breaks_from_fixed_ones() {
    let body = (0..4).map(|k| prose(&SYN, 70 + k, 3, 10, true)).collect::<Vec<_>>().join("\n\n");
    let parent = format!("Hi Ann,\n\n{body}\n\nThanks,\nBo");
    let src = SourceLayout::whole(&parent);
    assert!(src.frames.head && src.frames.tail);
    let raw = format!("Hello Cy,\n\n{}\n\n{}\n\nBest,\nDee", prose(&ALT, 72, 3, 10, true), prose(&ALT, 73, 3, 10, true));
    let t = transfer(&raw, &none(), &src);
    assert_eq!(t.diag.units.len(), t.paragraphs.len());
    let roles: Vec<UnitRole> = t.diag.units.iter().map(|u| u.role).collect();
    assert_eq!(roles.first(), Some(&UnitRole::HeadFrame));
    assert_eq!(roles.last(), Some(&UnitRole::TailFrame));
    for (u, d) in t.paragraphs.iter().zip(&t.diag.units) {
        // each unit's source holds its first word
        let first = t.text[u.0..u.1].split_whitespace().next().unwrap();
        assert!(raw[d.source.raw.0..d.source.raw.1].contains(first), "{first}");
        if d.role == UnitRole::Body {
            assert_eq!(d.segment, Some(0));
        }
    }
    // the line-form path: one Line unit per line
    let lf = SourceLayout::whole("a b\nc d\ne f\ng h");
    assert!(lf.line_form);
    let t = transfer("x y\nz w\nv u", &none(), &lf);
    assert!(t.diag.units.iter().all(|u| u.role == UnitRole::Line) && t.diag.units.len() == 3);
}

// ------------------------------------------------------------------ count 1 and the tally

/// A synthetic email parent (greeting and sign-off frames, a punctuated
/// body) and a mirror with its own greeting, a list and a sign-off: the
/// landed licence screen rejects the list, amendment 7's screen is off.
fn email() -> (String, String) {
    let body = (0..3).map(|k| prose(&SYN, 80 + k, 4, 14, true)).collect::<Vec<_>>().join("\n\n");
    let parent = format!("Hi Bob,\n\n{body}\n\nThanks,\nSam\n");
    let child = format!(
        "Hello Bob,\n\n{}\nHere is what we still need before the meeting:\n- the signed contract from the legal team\n- two printed copies of the budget\n- the booking for the large room\n\n{}\n\nBest regards,\nSam",
        prose(&ALT, 90, 5, 14, true),
        prose(&ALT, 91, 4, 14, true)
    );
    (parent, child)
}

fn case<'a>(src: Source<'a>, raw: &'a str, conv: &'a Value, rec: Recorded<'a>, landed: Result<(bool, Vec<String>), Failure>, a7: Result<(bool, Vec<String>), Failure>) -> Case<'a> {
    Case { batch: "b", source: "local", class: "model_only", domain: "web", item_id: "i1", request_id: Some("r1"), src, raw, conv, recorded: rec, landed, a7, fin: Fin::Whole }
}

#[test]
fn count1_a_child_judged_differently_by_the_two_codes_is_counted_under_its_reasons() {
    engine::quiet_panics();
    let (parent, child) = email();
    let conv = none_json();
    let src = Source::Mirror { parent: &parent };
    let l = engine::simple_verdict(Code::Landed, src, &conv, &child).unwrap();
    let a = engine::simple_verdict(Code::A7, src, &conv, &child).unwrap();
    assert!(!l.accepted && a.accepted, "landed {:?} / a7 {:?}", l.reasons, a.reasons);
    let digest = sha_hex(l.text.as_bytes());
    let mut t = Tally::default();
    t.count(&case(src, &child, &conv, Recorded { accepted: l.accepted, reasons: &l.reasons, text_sha256: &digest }, Ok((l.accepted, l.reasons.clone())), Ok((a.accepted, a.reasons.clone()))));
    let cell = &t.cells["local|model_only|web"];
    assert_eq!((cell["children"], cell.get("c1_accepted_to_rejected").copied()), (1, Some(1)));
    let r = &t.reasons["local|model_only"];
    assert!(l.reasons.iter().all(|x| r.get(&format!("c1_accepted_to_rejected:{x}")) == Some(&1)), "{r:?}");
    // counts 2 and 3 under both codes: amendment 7 merges the greeting and leaves the markers mid-line
    assert_eq!(cell.get("c2_a7_greeting_merged_children").copied(), Some(1));
    assert_eq!(cell.get("c2_landed_greeting_merged_children").copied(), None, "the landed code keeps the source-framed greeting");
    assert!(cell.get("c3_a7_list_marker_lines_mid_line").copied().unwrap_or(0) >= 3, "{cell:?}");
    // a child judged alike under both codes is not counted as a change
    let mut t2 = Tally::default();
    t2.count(&case(src, &child, &conv, Recorded { accepted: l.accepted, reasons: &l.reasons, text_sha256: &digest }, Ok((l.accepted, l.reasons.clone())), Ok((l.accepted, l.reasons.clone()))));
    let c2 = &t2.cells["local|model_only|web"];
    assert_eq!((c2.get("c1_accepted_to_rejected"), c2.get("c1_both_rejected_same_reasons").copied()), (None, Some(1)));
    // rejected under amendment 7 only, and different reason sets
    let mut t3 = Tally::default();
    let acc: Vec<String> = Vec::new();
    let lacc = engine::simple_verdict(Code::Landed, src, &conv, &prose(&ALT, 92, 14, 13, true)).unwrap();
    let raw3 = prose(&ALT, 92, 14, 13, true);
    let d3 = sha_hex(lacc.text.as_bytes());
    assert!(lacc.accepted, "{:?}", lacc.reasons);
    t3.count(&case(src, &raw3, &conv, Recorded { accepted: true, reasons: &acc, text_sha256: &d3 }, Ok((true, Vec::new())), Ok((false, vec!["x_a7".into()]))));
    assert_eq!(t3.cells["local|model_only|web"].get("c1_rejected_to_accepted").copied(), Some(1));
    assert_eq!(t3.reasons["local|model_only"].get("c1_rejected_to_accepted:x_a7").copied(), Some(1));
    let mut t4 = Tally::default();
    t4.count(&case(src, &child, &conv, Recorded { accepted: false, reasons: &l.reasons, text_sha256: &digest }, Ok((false, l.reasons.clone())), Ok((false, vec!["x_a7".into()]))));
    assert_eq!(t4.cells["local|model_only|web"].get("c1_reasons_differ").copied(), Some(1));
    assert_eq!(t4.reasons["local|model_only"].get("c1_reasons_differ:a7_only:x_a7").copied(), Some(1));
    // an amendment-7 failure is counted by its code
    let mut t5 = Tally::default();
    t5.count(&case(src, &child, &conv, Recorded { accepted: false, reasons: &l.reasons, text_sha256: &digest }, Ok((false, l.reasons.clone())), Err(Failure::Panic("x/conventions.rs:9".into()))));
    assert_eq!(t5.reasons["local|model_only"].get("c1_a7_failed:panic:conventions.rs:9").copied(), Some(1));
}

#[test]
fn a_recorded_child_whose_digest_is_altered_is_refused_and_nothing_is_counted() {
    let (parent, child) = email();
    let conv = none_json();
    let src = Source::Mirror { parent: &parent };
    let l = engine::simple_verdict(Code::Landed, src, &conv, &child).unwrap();
    let bad = sha_hex(format!("{} ", l.text).as_bytes());
    let mut t = Tally::default();
    t.count(&case(src, &child, &conv, Recorded { accepted: l.accepted, reasons: &l.reasons, text_sha256: &bad }, Ok((l.accepted, l.reasons.clone())), Ok((true, Vec::new()))));
    assert_eq!(t.mismatched(), 1);
    assert_eq!(t.mismatches.get("rederived_text_differs").copied(), Some(1));
    let cell = &t.cells["local|model_only|web"];
    assert_eq!((cell.get("children"), cell.get("mismatch").copied()), (None, Some(1)));
    assert_eq!(t.mismatch_ids.len(), 1);
    assert_eq!(t.mismatch_ids[0]["item_id"], "i1");
    // the verdict recomputed through the wiring must be the recorded one
    let good = sha_hex(l.text.as_bytes());
    let mut t2 = Tally::default();
    t2.count(&case(src, &child, &conv, Recorded { accepted: l.accepted, reasons: &l.reasons, text_sha256: &good }, Ok((true, Vec::new())), Ok((true, Vec::new()))));
    assert_eq!(t2.mismatches.get("landed_verdict_differs").copied(), Some(1));
    // a final document that differs from the recorded digest
    let span_parent = format!("{} {}", prose(&SYN, 93, 2, 10, true), prose(&SYN, 94, 6, 10, true));
    let s = span_parent.find(&prose(&SYN, 94, 6, 10, true)).unwrap();
    let ssrc = Source::Span { parent: &span_parent, span: (s, span_parent.len()) };
    let raw = prose(&ALT, 95, 6, 10, true);
    let v = engine::simple_verdict(Code::Landed, ssrc, &conv, &raw).unwrap();
    let vd = sha_hex(v.text.as_bytes());
    let mut c = case(ssrc, &raw, &conv, Recorded { accepted: v.accepted, reasons: &v.reasons, text_sha256: &vd }, Ok((v.accepted, v.reasons.clone())), Ok((v.accepted, v.reasons.clone())));
    c.fin = Fin::Splice { parent: &span_parent, span: (s, span_parent.len()), doc_sha256: Some("0") };
    let mut t3 = Tally::default();
    t3.count(&c);
    assert_eq!(t3.mismatches.get("final_document_differs").copied(), Some(1));
    // with the document's own digest it is counted
    let dd = sha_hex(v.doc.as_deref().unwrap().as_bytes());
    c.fin = Fin::Splice { parent: &span_parent, span: (s, span_parent.len()), doc_sha256: Some(&dd) };
    let mut t4 = Tally::default();
    t4.count(&c);
    assert_eq!((t4.mismatched(), t4.cells["local|model_only|web"]["children"]), (0, 1));
}

#[test]
fn the_ids_file_names_the_children_behind_a_non_zero_invariant_and_the_gate_reads_the_broadest_reading() {
    let parent = closing_parent(10, 30);
    let src = Source::Mirror { parent: &parent };
    let raw = format!("{}\n\n{}\n\n{}", prose(&ALT, 42, 1, 10, true), prose(&ALT, 43, 1, 10, true), prose(&ALT, 44, 1, 10, false));
    let conv = none_json();
    let l = engine::simple_verdict(Code::Landed, src, &conv, &raw).unwrap();
    let d = sha_hex(l.text.as_bytes());
    let mut t = Tally::default();
    t.count(&case(src, &raw, &conv, Recorded { accepted: l.accepted, reasons: &l.reasons, text_sha256: &d }, Ok((l.accepted, l.reasons.clone())), Ok((l.accepted, l.reasons.clone()))));
    assert_eq!(t.mismatched(), 0);
    let j = t.to_json();
    assert_eq!(j["gate"]["count5_kept_unit_sharing_a_unit"]["gate"], 1, "{}", j["gate"]);
    assert_eq!(j["gate"]["all_zero"], false);
    assert_eq!(t.ids.len(), 1);
    assert_eq!(t.ids[0]["what"], "count5_kept_unit_shares_a_unit");
    assert_eq!(t.ids[0]["detail"]["kept_units"][0][1], "closed");
    // the ids hold no text
    let s = serde_json::to_string(&t.ids).unwrap();
    for w in raw.split_whitespace() {
        assert!(!s.contains(w), "{w}");
    }
    // merging two batches adds their cells
    let b = grammar_a_dryrun::batch_record("b", &json!({}), &json!({}), &json!({}), &t, json!({}));
    let m = tally::merge(&[b.clone(), b]);
    assert_eq!(m["totals"]["all"]["children"], 2);
    assert_eq!(m["gate"]["count5_kept_unit_sharing_a_unit"]["gate"], 2);
}

/// A hard-wrapped parent at W 40 of `paras` paragraphs (4 sentences of 9
/// words each).
fn wrapped_parent(paras: u64) -> String {
    let mut lines = Vec::new();
    for k in 0..paras {
        let p = prose(&SYN, 20 + k, 4, 9, true);
        let mut cur = String::new();
        for w in p.split(' ') {
            if !cur.is_empty() && cur.chars().count() + 1 + w.chars().count() > 40 {
                lines.push(std::mem::take(&mut cur));
            }
            if !cur.is_empty() {
                cur.push(' ');
            }
            cur.push_str(w);
        }
        lines.push(cur);
        lines.push(String::new());
    }
    lines.join("\n").trim_end().to_string()
}

/// The words another edit puts on the span's first line before it.
const OTHER_EDIT: &str = "and twelve more words of another edit ";

/// A span child of the wrapped parent: the second paragraph's first
/// sentence (at a line start) replaced; its final document with another
/// edit's words before the span on its first line, so only the document
/// reading puts that line over its budget. The child starts at
/// `span.0 + OTHER_EDIT.len()` in the document.
fn doc_reading_case(parent: &str) -> (Source<'_>, String, engine::Verdict, String) {
    let s = parent.find(&prose(&SYN, 21, 1, 9, true)[..20]).unwrap();
    let e = s + parent[s..].find('.').unwrap() + 1;
    let src = Source::Span { parent, span: (s, e) };
    let raw = prose(&ALT, 31, 2, 9, true);
    let v = engine::simple_verdict(Code::Landed, src, &none_json(), &raw).unwrap();
    let doc = format!("{}{OTHER_EDIT}{}{}", &parent[..s], v.text, &parent[e..]);
    (src, raw, v, doc)
}

#[test]
fn the_gate_reads_each_invariants_broadest_reading_on_its_population() {
    let conv = none_json();
    // (i) a span child whose first paragraph is under 8 words alone but not with the parent words it joins
    let p1 = format!("{} {}", prose(&SYN, 60, 1, 6, true), prose(&SYN, 61, 1, 4, true));
    let parent = format!("{p1}\n\n{}\n\n{}", prose(&SYN, 62, 3, 10, true), prose(&SYN, 63, 2, 10, true));
    let s = parent.find(&prose(&SYN, 61, 1, 4, true)).unwrap();
    let e = parent.len();
    let src = Source::Span { parent: &parent, span: (s, e) };
    let raw = format!("{} {} {}", prose(&ALT, 64, 1, 3, true), prose(&ALT, 65, 4, 10, true), prose(&ALT, 66, 2, 10, true));
    let v = engine::simple_verdict(Code::Landed, src, &conv, &raw).unwrap();
    let d = sha_hex(v.text.as_bytes());
    let mut c = case(src, &raw, &conv, Recorded { accepted: v.accepted, reasons: &v.reasons, text_sha256: &d }, Ok((v.accepted, v.reasons.clone())), Ok((v.accepted, v.reasons.clone())));
    c.fin = Fin::Splice { parent: &parent, span: (s, e), doc_sha256: None };
    let mut t = Tally::default();
    t.count(&c);
    assert_eq!(t.mismatched(), 0, "{:?}", t.mismatches);
    let g = &t.to_json()["gate"];
    let c6 = &g["count6_placed_break_paragraph_under_8_words"];
    assert_eq!((c6["gate"].as_u64(), c6["final_document_reading"].as_u64()), (Some(1), Some(0)), "{g}");
    assert_eq!(g["all_zero"], false);
    assert_eq!(t.ids.iter().filter(|x| x["what"] == "count6_placed_break_paragraph_under_8_words").count(), 1);
    // (ii) an accepted line over its budget in the document reading only gates count 4
    let wp = wrapped_parent(5);
    let (src, raw, v, doc) = doc_reading_case(&wp);
    assert!(v.accepted, "{:?}", v.reasons);
    let Source::Span { span: (s, _), .. } = src else { unreachable!() };
    let d = sha_hex(v.text.as_bytes());
    let mut c = case(src, &raw, &conv, Recorded { accepted: true, reasons: &v.reasons, text_sha256: &d }, Ok((true, v.reasons.clone())), Ok((true, v.reasons.clone())));
    let at = s + OTHER_EDIT.len();
    c.fin = Fin::Doc(&doc, (at, at + v.text.len()));
    let mut t = Tally::default();
    t.count(&c);
    assert_eq!(t.mismatched(), 0, "{:?}", t.mismatches);
    let g = &t.to_json()["gate"]["count4_accepted_wrapped_lines_over_budget"];
    assert_eq!((g["replacement_reading_accepted"].as_u64(), g["document_reading_accepted"].as_u64()), (Some(0), Some(1)), "{g}");
    assert_eq!((g["gate"].as_u64(), g["children"].as_u64()), (Some(1), Some(1)), "{g}");
    assert_eq!(t.ids.iter().filter(|x| x["what"] == "count4_wrapped_line_over_budget").count(), 1);
    // (iii) the same geometry on a rejected child (a parent under 150 words): every child's figure, not the gate
    let wp = wrapped_parent(3);
    let (src, raw, v, doc) = doc_reading_case(&wp);
    assert!(!v.accepted, "{:?}", v.reasons);
    let Source::Span { span: (s, _), .. } = src else { unreachable!() };
    let d = sha_hex(v.text.as_bytes());
    let mut c = case(src, &raw, &conv, Recorded { accepted: false, reasons: &v.reasons, text_sha256: &d }, Ok((false, v.reasons.clone())), Ok((false, v.reasons.clone())));
    let at = s + OTHER_EDIT.len();
    c.fin = Fin::Doc(&doc, (at, at + v.text.len()));
    let mut t = Tally::default();
    t.count(&c);
    assert_eq!(t.mismatched(), 0, "{:?}", t.mismatches);
    let g = &t.to_json()["gate"]["count4_accepted_wrapped_lines_over_budget"];
    assert_eq!((g["gate"].as_u64(), g["children"].as_u64(), g["every_child_document_reading"].as_u64()), (Some(0), Some(0), Some(1)), "{g}");
    assert!(t.ids.is_empty(), "{:?}", t.ids);
}

#[test]
fn reasons_built_from_error_messages_are_cut() {
    assert_eq!(tally::safe_reason("window_assembly_error:span order"), "window_assembly_error:span order");
    assert_eq!(tally::safe_reason("tokenizer_error:\u{201C}quoted text\u{201D}"), "tokenizer_error:<other>");
}
