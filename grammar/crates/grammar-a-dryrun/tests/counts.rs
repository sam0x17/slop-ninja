//! B23's counting kernel on synthetic text only (no corpus text, no live
//! call): each stop invariant's counted case and its near misses, counts 1-3
//! on amendment 7's code against the landed code, the re-derivation
//! identity and the merge.
use grammar_a_dryrun::engine::{self, Code, Failure, Source};
use grammar_a_dryrun::invariants::{Final, invariants};
use grammar_a_dryrun::tally::{self, Case, Fin, Recorded, Tally};
use grammar_a_dryrun::{OwnSplit, Split, TRAIN_SIDE_DERIVED, line_split, request_split, sha_hex};
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

#[test]
fn a_request_lines_split_is_its_own_top_level_key() {
    // Amendment 32 item 32.2's set, on synthetic lines.
    let derived = br#"{"request_id":"r1","split":"train","voice":{"derived_from":{"split":"train_mining","id":"h1"},"text":"a \"quoted\" split"}}"#;
    assert_eq!(request_split(derived), OwnSplit::One { split: "train".into(), derived: Some("train_mining".into()) });
    // the old reader reads every split key and refuses this line
    assert_eq!(line_split(derived, "split"), Split::Unreadable);
    assert!(TRAIN_SIDE_DERIVED.contains(&"train_mining") && TRAIN_SIDE_DERIVED.contains(&"train") && TRAIN_SIDE_DERIVED.len() == 2);
    // a derived dev_select is returned for the caller's guard (it refuses
    // the plan; examples/a_frontier_positives/count.rs (omitted from the public copy))
    let dev = br#"{"split":"train","voice":{"derived_from":{"split":"dev_select"}}}"#;
    assert_eq!(request_split(dev), OwnSplit::One { split: "train".into(), derived: Some("dev_select".into()) });
    assert!(!TRAIN_SIDE_DERIVED.contains(&"dev_select"));
    // skipped unparsed: a top-level train_mining (its own split), a
    // repeated top-level split, a split at another nested path, an escaped
    // or non-string value
    assert_eq!(request_split(br#"{"split":"train_mining"}"#), OwnSplit::One { split: "train_mining".into(), derived: None });
    assert_eq!(request_split(br#"{"split":"train","split":"train"}"#), OwnSplit::Unreadable);
    assert_eq!(request_split(br#"{"split":"train","meta":{"split":"train"}}"#), OwnSplit::Unreadable);
    assert_eq!(request_split(br#"{"split":"train","voice":{"split":"train"}}"#), OwnSplit::Unreadable);
    assert_eq!(request_split(br#"{"split":"train","voice":[{"derived_from":{"split":"train"}}]}"#), OwnSplit::Unreadable);
    assert_eq!(request_split(br#"{"split":"tr\u0061in"}"#), OwnSplit::Unreadable);
    assert_eq!(request_split(br#"{"split":1}"#), OwnSplit::Unreadable);
    assert_eq!(request_split(br#"{"split":"train","voice":{"derived_from":{"split":null}}}"#), OwnSplit::DerivedUnreadable { split: Some("train".into()) });
    assert_eq!(request_split(br#"{"split":"train","voice":{"derived_from":{"split":"dev\u005fselect"}}}"#), OwnSplit::DerivedUnreadable { split: Some("train".into()) });
    assert_eq!(request_split(br#"{"voice":{"derived_from":{"split":"a","split":"b"}}}"#), OwnSplit::DerivedUnreadable { split: None });
    assert_eq!(request_split(br#"{"split":"cal","voice":{"derived_from":{"split":1}}}"#), OwnSplit::DerivedUnreadable { split: Some("cal".into()) });
    // an unreadable own split or a split at another path stays Unreadable
    assert_eq!(request_split(br#"{"split":1,"voice":{"derived_from":{"split":null}}}"#), OwnSplit::Unreadable);
    assert_eq!(request_split(br#"{"split":"train","meta":{"split":"x"},"voice":{"derived_from":{"split":null}}}"#), OwnSplit::Unreadable);
    assert_eq!(request_split(br#"{"spl\u0069t":"train","split":"train"}"#), OwnSplit::Unreadable);
    // a split word inside a string value is no key
    assert_eq!(request_split(br#"{"x":"\"split\":\"test\"","split":"train"}"#), OwnSplit::One { split: "train".into(), derived: None });
    assert_eq!(request_split(br#"{"x":1, "y":[true,null,{"z":"w"}]}"#), OwnSplit::Missing { derived: None });
    assert_eq!(request_split(br#"{"voice":{"derived_from":{"split":"train_mining"}}}"#), OwnSplit::Missing { derived: Some("train_mining".into()) });
    // a line that is not one JSON object is read as line_split reads it
    // (every split key; a Train line then fails its parse)
    for bad in [&br#"["split","train"]"#[..], br#"{"split":"train""#, br#"{"split":"train"} {}"#, b"", b"{\"split\":\"cal\",\x00", br#"{"split":"train","passage":{"split":"train_mining"}, broken"#] {
        let want = match line_split(bad, "split") {
            Split::One(split) => OwnSplit::One { split, derived: None },
            Split::Missing => OwnSplit::Missing { derived: None },
            Split::Unreadable => OwnSplit::Unreadable,
        };
        assert_eq!(request_split(bad), want, "{:?}", String::from_utf8_lossy(bad));
    }
    assert_eq!(request_split(br#"{"split":"train""#), OwnSplit::One { split: "train".into(), derived: None });
    assert_eq!(request_split(b"{\"split\":\"cal\",\x00"), OwnSplit::One { split: "cal".into(), derived: None });
    assert_eq!(request_split(b" {\"split\" : \"train\" }\r"), OwnSplit::One { split: "train".into(), derived: None });
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
fn count4_an_items_edits_are_re_derived_in_their_item_and_measured_there() {
    // Amendment 29 item 29.1: an item's edit is re-derived in its item
    // (`Source::InItem`), giving the item's text; a later edit with no room
    // on its shared line starts on a new line, which is not measured from
    // the column (count 4 reads no line over W).
    use grammar_a_dryrun::engine::Slot;
    use grammar_a_form::profile::{EditV2, item_v2, replacement_lead};
    let conv = none_json();
    let (mut seen, mut zero_room, mut by_29_2) = (0, 0, 0);
    for seed in 0..300u64 {
        let paras: Vec<String> = (0..2).map(|k| grammar_a_form::transfer::wrap(&prose(&SYN, seed * 3 + k, 12, 4, true), 60, 0).join("\n")).collect();
        let q = paras.join("\n\n");
        let sents: Vec<(usize, usize)> = grammar_a_form::text::sentences(&q).iter().map(|s| (s.start, s.end)).collect();
        let Some((a, b)) = (0..sents.len()).flat_map(|a| (a + 2..sents.len()).map(move |b| (a, b))).find(|&(a, b)| !q[sents[a].1..sents[b].0].contains('\n')) else { continue };
        let (r1, r2) = (prose(&ALT, seed + 900, 1 + (seed % 3) as usize, 4, true), prose(&ALT, seed + 901, 1 + (seed % 2) as usize, 4, true));
        let e = [EditV2 { span: sents[a], insert_sep: None, raw: &r1 }, EditV2 { span: sents[b], insert_sep: None, raw: &r2 }];
        let it = item_v2(&q, &none(), &e);
        let Some(asm) = &it.assembled else { continue };
        seen += 1;
        zero_room += it.record["edits"][1]["transfer"]["sibling_column_wraps"].as_u64().unwrap_or(0) as usize;
        let slots = [Slot { span: sents[a], insert_sep: None, raw: Some(&r1) }, Slot { span: sents[b], insert_sep: None, raw: Some(&r2) }];
        for (k, ek) in e.iter().enumerate() {
            let src = Source::InItem { parent: &q, slots: &slots, index: k };
            assert_eq!(src.kind(), "mixed");
            let (sl, t) = engine::landed_transfer(src, &conv, ek.raw).unwrap();
            assert_eq!(t.text, it.reps[k], "seed {seed} edit {k}");
            let fin = (asm.edit_spans[k].0 - replacement_lead(&it.reps[k]).unwrap(), asm.edit_spans[k].1);
            let inv = invariants(&sl, &t, Some(Final { doc: &asm.text, fin }));
            assert!(inv.inconsistent.is_none());
            // amendment 29 item 29.2 (as item 34.2 (d) foresees): a document
            // line over W holding an edit's one-word line after parent text
            // counts now; every such line is one the item-29.2 reading adds
            assert!(inv.over_budget.is_empty(), "seed {seed} edit {k}: {inv:?}");
            assert_eq!(inv.over_budget_document.len(), inv.by_document_line_words, "seed {seed} edit {k}: {inv:?}");
            // item 34.2 (d)'s foreseen case only: no earlier edit's laid-out
            // text on the counted document line (a failure of item 29.1's
            // no-room rule would leave an earlier sibling's last piece there)
            for l in inv.over_budget_document.iter() {
                let l0 = l.doc_line.expect("a final document");
                let l1 = asm.text[l0..].find('\n').map_or(asm.text.len(), |x| l0 + x);
                for j in 0..k {
                    let fj = (asm.edit_spans[j].0 - replacement_lead(&it.reps[j]).unwrap(), asm.edit_spans[j].1);
                    assert!(!(l0 < fj.1 && fj.0 < l1), "seed {seed} edit {k}: the counted document line holds edit {j}'s text");
                }
            }
            by_29_2 += inv.by_document_line_words;
        }
        // amendment 7's code lays the edit out alone
        let a7 = engine::laid(Code::A7, Source::InItem { parent: &q, slots: &slots, index: 1 }, &conv, &r2).unwrap();
        assert_eq!(a7.text, engine::laid(Code::A7, Source::Span { parent: &q, span: sents[b] }, &conv, &r2).unwrap().text);
    }
    assert!(seen >= 100 && zero_room >= 1, "{seen} {zero_room}");
    println!("document-line one-word lines counted by item 29.2: {by_29_2}");
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
    // Count 5's shape (log index 101): the break target (a quarter of the
    // way) falls before the second paragraph. Before amendment 30 the
    // closed third paragraph shared its unit with the second; its start is
    // now a fixed boundary (item 30.1), so the landed transfer keeps it
    // its own unit.
    let parent = closing_parent(10, 30);
    let src = SourceLayout::whole(&parent);
    assert!(src.licence.iter().any(|r| r == "no_sentence_end") && !src.line_form, "{:?}", src.licence);
    let raw = format!("{}\n\n{}\n\n{}", prose(&ALT, 42, 1, 10, true), prose(&ALT, 43, 1, 10, true), prose(&ALT, 44, 1, 10, false));
    let mut t = transfer(&raw, &none(), &src);
    assert_eq!(t.record.closed_paragraphs, 1);
    let inv = invariants(&src, &t, None);
    assert_eq!((inv.closed_units, inv.kept_shared.len(), inv.closed_split), (1, 0, 0), "{:?}", t.diag);
    // the same child with the closed unit drawn into the unit before it
    // (the layout before amendment 30) is counted
    let k = t.diag.kept.iter().position(|k| k.kind == KeptKind::Closed).unwrap();
    let kr = t.diag.kept[k].raw;
    let u = t.diag.units.iter().position(|u| u.source.raw == kr).unwrap();
    t.diag.units[u - 1].source.raw.1 = kr.1;
    t.diag.units.remove(u);
    t.paragraphs.remove(u);
    let inv = invariants(&src, &t, None);
    assert_eq!(inv.kept_shared, vec![k]);
}

#[test]
fn count5_a_target_inside_a_closed_paragraph_places_no_break() {
    // Item 30.3: closed_split is 0 by construction. The source's targets
    // fall at a quarter (the closed start) and a half (inside it).
    let parent = format!("{}\n\n{}\n\n{}", prose(&SYN, 46, 1, 10, true), prose(&SYN, 47, 1, 10, true), prose(&SYN, 48, 2, 10, false));
    let src = SourceLayout::whole(&parent);
    let raw = format!("{}\n\n{}", prose(&ALT, 49, 1, 10, true), prose(&ALT, 50, 3, 10, false));
    let t = transfer(&raw, &none(), &src);
    let inv = invariants(&src, &t, None);
    assert_eq!((inv.closed_units, inv.kept_shared.len(), inv.closed_split), (1, 0, 0), "{:?}", t.diag);
    assert_eq!(t.record.targets_on_fixed_boundaries, 2);
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
    Case { batch: "b", source: "local", class: "model_only", domain: "web", item_id: "i1", request_id: Some("r1"), src, raw, conv, recorded: rec, landed, a7, fin: Fin::Whole, item: None, remeasure_differs: Some(true) }
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
    // Amendment 31 moved count 6's gate to the constructed paragraph, so
    // B23's count-6 pin child no longer gates
    // (count6_through_the_tally_gates_on_the_construction...); the ids
    // file is pinned on count 4's document reading instead.
    let conv = none_json();
    let wp = wrapped_parent(5);
    let (src, raw, v, doc) = doc_reading_case(&wp);
    let Source::Span { span: (s, _), .. } = src else { unreachable!() };
    let d = sha_hex(v.text.as_bytes());
    let mut c = case(src, &raw, &conv, Recorded { accepted: true, reasons: &v.reasons, text_sha256: &d }, Ok((true, v.reasons.clone())), Ok((true, v.reasons.clone())));
    let at = s + OTHER_EDIT.len();
    c.fin = Fin::Doc(&doc, (at, at + v.text.len()));
    let mut t = Tally::default();
    t.count(&c);
    assert_eq!(t.mismatched(), 0);
    let j = t.to_json();
    assert_eq!(j["gate"]["count4_accepted_wrapped_lines_over_budget"]["gate"], 1, "{}", j["gate"]);
    assert_eq!(j["gate"]["all_zero"], false);
    assert_eq!(t.ids.len(), 1);
    assert_eq!(t.ids[0]["what"], "count4_wrapped_line_over_budget");
    assert_eq!(t.ids[0]["kind"], "mixed");
    // the ids hold no text
    let s = serde_json::to_string(&t.ids).unwrap();
    for w in raw.split_whitespace() {
        assert!(!s.contains(w), "{w}");
    }
    // merging two batches adds their cells
    let b = grammar_a_dryrun::batch_record("b", &json!({}), &json!({}), &json!({}), &t, json!({}));
    let m = tally::merge(&[b.clone(), b]);
    assert_eq!(m["totals"]["all"]["children"], 2);
    assert_eq!(m["gate"]["count4_accepted_wrapped_lines_over_budget"]["gate"], 2);
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
    // amendment 31: the constructed paragraph (9 words) gates, the own
    // words (3) are reported beside it
    assert_eq!((c6["gate"].as_u64(), c6["own_words_reading"].as_u64(), c6["final_document_reading"].as_u64()), (Some(0), Some(1), Some(0)), "{g}");
    assert_eq!(g["all_zero"], true);
    assert_eq!(t.ids.iter().filter(|x| x["what"] == "count6_placed_break_paragraph_under_8_words").count(), 0);
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

// ------------------------------------------------------------------ B27: count 4's document line (amendment 29 item 29.2)

#[test]
fn count4_a_document_line_of_two_one_word_pieces_over_w_is_counted() {
    // The child's first line is one word that fits at its column; the
    // earlier edit's one-word last piece shares the document line, which is
    // over W (25 columns at W 20) with two words: counted under item 29.2,
    // exempt under the replacement reading (the child's own text on the
    // line holds one word).
    let (mut src, t) = hand(&[("bbbbbbbbbbbb\ncc dd", B, true, Some(0))], Some(20), true);
    src.column = 0;
    let doc = format!("x\naaaaaaaaaaaa {}\nzz", t.text);
    let at = "x\naaaaaaaaaaaa ".len();
    let inv = invariants(&src, &t, Some(Final { doc: &doc, fin: (at, at + t.text.len()) }));
    assert_eq!(inv.over_budget.len(), 0, "{inv:?}");
    assert_eq!(inv.over_budget_document.iter().map(|x| (x.unit, x.line, x.doc_line)).collect::<Vec<_>>(), vec![(0, 0, Some(2))]);
    assert_eq!(inv.by_document_line_words, 1);
    // near miss: a document line over W that holds one word is exempt
    let (src, t) = hand(&[("bbbbbbbbbbbbbbbbbbbbbbbbb\ncc dd", B, true, Some(0))], Some(20), true);
    let doc = format!("x\n{}\nzz", t.text);
    let inv = invariants(&src, &t, Some(Final { doc: &doc, fin: (2, 2 + t.text.len()) }));
    assert!(inv.over_budget_document.is_empty() && inv.by_document_line_words == 0, "{inv:?}");
    // near miss: two words on a document line of exactly W columns
    let (src, t) = hand(&[("bbbbbbb\ncc dd", B, true, Some(0))], Some(20), true);
    let doc = format!("aaaaaaaaaaaa {}", t.text);
    let inv = invariants(&src, &t, Some(Final { doc: &doc, fin: (13, 13 + t.text.len()) }));
    assert!(inv.over_budget_document.is_empty(), "20 columns is within W: {inv:?}");
}

#[test]
fn count4_distinct_document_lines_are_counted_once_beside_child_lines() {
    let conv = none_json();
    let wp = wrapped_parent(5);
    let (src, raw, v, doc) = doc_reading_case(&wp);
    let Source::Span { span: (s, _), .. } = src else { unreachable!() };
    let d = sha_hex(v.text.as_bytes());
    let at = s + OTHER_EDIT.len();
    let mut t = Tally::default();
    for item in ["i1", "i1", "i2"] {
        let mut c = case(src, &raw, &conv, Recorded { accepted: true, reasons: &v.reasons, text_sha256: &d }, Ok((true, v.reasons.clone())), Ok((true, v.reasons.clone())));
        c.item_id = item;
        c.fin = Fin::Doc(&doc, (at, at + v.text.len()));
        t.count(&c);
    }
    assert_eq!(t.mismatched(), 0, "{:?}", t.mismatches);
    let g = &t.to_json()["gate"]["count4_accepted_wrapped_lines_over_budget"];
    // the same document line of one item counted by two of its children is one document line
    assert_eq!((g["gate"].as_u64(), g["document_lines"].as_u64(), g["every_child_document_reading_document_lines"].as_u64()), (Some(3), Some(2), Some(2)), "{g}");
}

// ------------------------------------------------------------------ B27: count 5's closed_split flag (item 30.3)

#[test]
fn count5_a_closed_paragraph_split_into_two_units_is_reported_and_flagged() {
    let parent = closing_parent(10, 10);
    let src = SourceLayout::whole(&parent);
    let raw = format!("{}\n\n{}", prose(&ALT, 45, 1, 10, true), prose(&ALT, 46, 1, 10, false));
    let mut t = transfer(&raw, &none(), &src);
    assert_eq!(invariants(&src, &t, None).closed_split, 0, "0 by construction");
    // the closed unit drawn by two units (the layout item 30.1 forbids)
    let k = t.diag.kept.iter().position(|k| k.kind == KeptKind::Closed).unwrap();
    let kr = t.diag.kept[k].raw;
    let u = t.diag.units.iter().position(|u| u.source.raw == kr).unwrap();
    let mid = kr.0 + (kr.1 - kr.0) / 2;
    let mut second = t.diag.units[u];
    t.diag.units[u].source.raw.1 = mid;
    second.source.raw.0 = mid;
    let (a, b) = t.paragraphs[u];
    let cut = a + t.text[a..b].find(' ').unwrap();
    t.paragraphs[u] = (a, cut);
    t.paragraphs.insert(u + 1, (cut + 1, b));
    t.diag.units.insert(u + 1, second);
    let inv = invariants(&src, &t, None);
    assert_eq!((inv.closed_split, inv.kept_shared.len()), (1, 0), "{:?}", t.diag);
    // the gate reports and flags it, and it gates nothing
    let mut cells = std::collections::BTreeMap::new();
    cells.insert("local|model_only|web".to_string(), std::collections::BTreeMap::from([("c5_closed_split".to_string(), 1u64)]));
    let g = tally::gate(&tally::totals(&cells));
    assert_eq!((g["count5_kept_unit_sharing_a_unit"]["closed_split"].as_u64(), g["count5_kept_unit_sharing_a_unit"]["closed_split_flagged"].as_bool()), (Some(1), Some(true)));
    assert_eq!((g["flags"]["closed_split_not_zero"].as_bool(), g["all_zero"].as_bool()), (Some(true), Some(true)));
    let g0 = tally::gate(&tally::totals(&std::collections::BTreeMap::new()));
    assert_eq!(g0["flags"]["closed_split_not_zero"], false);
}

// ------------------------------------------------------------------ B27: count 6 on the constructed paragraph (amendment 31)

use grammar_a_dryrun::count6::{self, ItemDoc, ItemEdit, Own};
use grammar_a_dryrun::tally::{Gate6, gate6};

/// A span child of `parent` at `span` with hand-made body units (text,
/// segment) joined by blank lines: its source is the landed
/// `SourceLayout::span`, so its `pre_words` and `post_words` are the
/// parent's.
fn hand_span(parent: &str, span: (usize, usize), units: &[(&str, Option<usize>)]) -> (SourceLayout, Transferred) {
    let src = SourceLayout::span(parent, span.0, span.1).unwrap();
    let mut t = transfer("Acorn bramble canyon dune estuary fern glacier heather.", &none(), &src);
    let (mut text, mut paragraphs, mut diag) = (String::new(), Vec::new(), Vec::new());
    for (k, (u, segment)) in units.iter().enumerate() {
        if k > 0 {
            text.push_str("\n\n");
        }
        let a = text.len();
        text.push_str(u);
        paragraphs.push((a, text.len()));
        diag.push(UnitDiag { role: B, wrapped: false, segment: *segment, source: ParagraphSource { raw: (k * 100, k * 100 + 50), starts_mid_line: false, ends_mid_line: false } });
    }
    t.text = text;
    t.paragraphs = paragraphs;
    t.diag = TransferDiag { units: diag, kept: Vec::new() };
    (src, t)
}

const FIVE: &str = "Acorn bramble canyon dune estuary.";
const TEN: &str = "Fern glacier heather inlet jasper knoll lichen marsh nettle oasis.";

/// A parent whose first paragraph holds `pre` words, then the span's
/// sentence, then a second paragraph.
fn span_parent(pre: usize) -> (String, (usize, usize)) {
    let lead = if pre == 0 { String::new() } else { format!("{} ", prose(&SYN, 3, 1, pre, true)) };
    let old = "Old span sentence here.";
    let p = format!("{lead}{old}\n\n{}", prose(&SYN, 4, 2, 10, true));
    let s = lead.len();
    (p, (s, s + old.len()))
}

#[test]
fn count6_an_edge_unit_of_5_own_words_in_a_constructed_paragraph_of_12_is_not_counted_and_of_7_is() {
    // 12 and 7 words (BUILD-PLAN 13.5's pins), and the floor itself: 8 is not under 8
    for (pre, want_short) in [(7, false), (2, true), (3, false)] {
        let (parent, span) = span_parent(pre);
        let (src, t) = hand_span(&parent, span, &[(FIVE, Some(0)), (TEN, Some(0))]);
        assert_eq!(src.structure.pre_words, pre, "the parent's words before the span");
        let c = count6::count6(&src, &t, Some(Own { parent: &parent, span, sep: None }), None);
        assert!(c.inconsistent.is_none());
        let u0 = &c.units[0];
        assert!(u0.placed && u0.own == 5);
        assert_eq!(u0.alone, Some(5 + pre), "the constructed paragraph joins the parent's words before the span (12, 7, 8)");
        assert_eq!(u0.item, None);
        assert_eq!(u0.short, want_short, "pre {pre}: {u0:?}");
        assert!(!u0.differs, "the arithmetic agrees with the construction here");
        let g = gate6(&c, Some(true));
        assert_eq!(g.gate, if want_short { vec![0] } else { vec![] });
        // item 28.9's own-words reading counts both; it gates nothing
        assert_eq!(c.units.iter().filter(|x| x.placed && x.own < 8).count(), 1);
    }
}

#[test]
fn count6_a_sibling_span_cutting_the_join_ends_the_paragraph_at_the_siblings_unit_boundary() {
    // parent paragraph: eight words, a sibling's sentence, then the child's
    // sentence; the sibling's replacement has two units, its last of two
    // words, so the child's first unit (5 words) is in a constructed
    // paragraph of 2 + 5 = 7 words in the item, though 17 alone
    let lead = format!("{} ", prose(&SYN, 5, 1, 8, true));
    let sib_old = "Sibling sentence of four.";
    let own_old = "Old span sentence here.";
    let parent = format!("{lead}{sib_old} {own_old}\n\n{}", prose(&SYN, 6, 2, 10, true));
    let sib_span = (lead.len(), lead.len() + sib_old.len());
    let own_span = (sib_span.1 + 1, sib_span.1 + 1 + own_old.len());
    let (src, t) = hand_span(&parent, own_span, &[(FIVE, Some(0)), (TEN, Some(0))]);
    let sib = format!("{TEN}\n\nQuarry ridge.");
    let sib_units = vec![(0, TEN.len()), (TEN.len() + 2, sib.len())];
    let parts = [grammar_a_form::profile::Part { span: sib_span, insert_sep: None }, grammar_a_form::profile::Part { span: own_span, insert_sep: None }];
    let a = grammar_a_form::profile::assemble_item(&parent, &parts, &[&sib, &t.text]).unwrap();
    let edits = vec![
        ItemEdit { span: sib_span, sep: None, fin: a.edit_spans[0], units: sib_units },
        ItemEdit { span: own_span, sep: None, fin: a.edit_spans[1], units: t.paragraphs.clone() },
    ];
    let own = Own { parent: &parent, span: own_span, sep: None };
    let c = count6::count6(&src, &t, Some(own), Some(ItemDoc { parent: &parent, doc: &a.text, edits: &edits, index: 1 }));
    assert!(c.inconsistent.is_none(), "{c:?}");
    let u0 = &c.units[0];
    assert_eq!((u0.item, u0.alone), (Some(7), Some(5 + 8 + 4)), "{u0:?}");
    assert!(u0.short, "either reading under 8 counts");
    assert!(u0.differs, "pre_words counts the replaced parent words");
    assert_eq!(gate6(&c, Some(true)), Gate6 { gate: vec![0], constructed: vec![0], failsafe: vec![], remeasured: vec![0], differs: vec![0], figure1: true });
    // the re-measured reading reads the item: "Quarry ridge." and the unit are one 7-word paragraph there too
    assert!(u0.remeasured_short);
    // the floor on the item reading: a sibling's last unit of 3 words gives 8, not under 8
    let sib8 = format!("{TEN}\n\nQuarry ridge sorrel.");
    let a8 = grammar_a_form::profile::assemble_item(&parent, &parts, &[&sib8, &t.text]).unwrap();
    let edits8 = vec![
        ItemEdit { span: sib_span, sep: None, fin: a8.edit_spans[0], units: vec![(0, TEN.len()), (TEN.len() + 2, sib8.len())] },
        ItemEdit { span: own_span, sep: None, fin: a8.edit_spans[1], units: t.paragraphs.clone() },
    ];
    let c8 = count6::count6(&src, &t, Some(own), Some(ItemDoc { parent: &parent, doc: &a8.text, edits: &edits8, index: 1 }));
    assert_eq!((c8.units[0].item, c8.units[0].short), (Some(8), false), "{:?}", c8.units[0]);
    // with a one-unit sibling the join runs through its text to the parent's boundary
    let sib1 = TEN.to_string();
    let a1 = grammar_a_form::profile::assemble_item(&parent, &parts, &[&sib1, &t.text]).unwrap();
    let edits1 = vec![
        ItemEdit { span: sib_span, sep: None, fin: a1.edit_spans[0], units: vec![(0, sib1.len())] },
        ItemEdit { span: own_span, sep: None, fin: a1.edit_spans[1], units: t.paragraphs.clone() },
    ];
    let c1 = count6::count6(&src, &t, Some(own), Some(ItemDoc { parent: &parent, doc: &a1.text, edits: &edits1, index: 1 }));
    assert_eq!(c1.units[0].item, Some(8 + 10 + 5));
    assert!(!c1.units[0].short);
    // an item document that does not hold the child's re-derived units is inconsistent
    let mut bad = edits.clone();
    bad[1].units = vec![(0, 3)];
    let cb = count6::count6(&src, &t, Some(own), Some(ItemDoc { parent: &parent, doc: &a.text, edits: &bad, index: 1 }));
    assert!(cb.inconsistent.is_some());
}

#[test]
fn count6_with_no_assembled_reading_falls_back_to_the_units_own_words_and_a_whole_text_is_the_unit_itself() {
    // a span outside the parent's token boundaries: neither reading assembles
    let (parent, span) = span_parent(6);
    let bad_span = (span.0 + 1, span.1);
    let (src, t) = hand_span(&parent, span, &[(FIVE, Some(0)), (TEN, Some(0))]);
    let c = count6::count6(&src, &t, Some(Own { parent: &parent, span: bad_span, sep: None }), None);
    assert_eq!((c.units[0].item, c.units[0].alone, c.units[0].short), (None, None, true), "{:?}", c.units[0]);
    // a whole text: its paragraph is the unit itself, and the arithmetic never differs
    let (wsrc, wt) = hand(&[("one two three four five six seven.", B, false, Some(0)), (TEN, B, false, Some(0))], None, false);
    let c = count6::count6(&wsrc, &wt, None, None);
    assert_eq!(c.units.iter().map(|x| (x.placed, x.short, x.differs)).collect::<Vec<_>>(), vec![(true, true, false), (true, false, false)]);
    // a short paragraph at a fixed boundary is read (item 31.2's figure) but is no placed unit
    let (wsrc, wt) = hand(&[("one two three.", B, false, Some(0)), (TEN, B, false, Some(1))], None, false);
    let c = count6::count6(&wsrc, &wt, None, None);
    assert_eq!(c.units.iter().map(|x| (x.placed, x.short)).collect::<Vec<_>>(), vec![(false, true), (false, false)]);
    assert!(gate6(&c, None).gate.is_empty());
}

#[test]
fn count6_a_remeasured_short_unit_is_figure_1s_only_when_its_record_shows_a_difference() {
    // one constructed unit of 12 words that re-measurement splits at its
    // blank line into 2 + 10 words (item 31.2's re-measured reading)
    let (src, t) = hand(&[("Acorn bramble.\n\nFern glacier heather inlet jasper knoll lichen marsh nettle oasis.", B, false, Some(0)), (TEN, B, false, Some(0))], None, false);
    let c = count6::count6(&src, &t, None, None);
    let u0 = &c.units[0];
    assert!(u0.placed && !u0.short && u0.remeasured_short, "{u0:?}");
    assert!(!c.units[1].remeasured_short);
    // figure 1's: the record shows a re-measured difference; not in the gate
    let g = gate6(&c, Some(true));
    assert_eq!((g.gate.clone(), g.remeasured.clone(), g.failsafe.clone(), g.figure1), (vec![], vec![0], vec![], true));
    // fail-safe: a record that does not show one, or holds no remeasure, counts it in the gate
    for d in [Some(false), None] {
        let g = gate6(&c, d);
        assert_eq!((g.gate.clone(), g.failsafe.clone(), g.figure1), (vec![0], vec![0], false), "{d:?}");
    }
    // the record reader: a child's own remeasure, an item's own edit
    assert_eq!(count6::remeasure_differs(&json!({"remeasure": {"differs": true}})), Some(true));
    assert_eq!(count6::remeasure_differs(&json!({"remeasure": null})), None);
    let item = json!({"edits": [{"remeasure": {"differs": false}}, {"remeasure": {"differs": true}}], "item_check": {"last": true, "index": 1}});
    assert_eq!(count6::remeasure_differs(&item), Some(true));
    let first = json!({"edits": [{"remeasure": {"differs": false}}], "item_check": {"last": false, "index": 1}});
    assert_eq!(count6::remeasure_differs(&first), Some(false));
    assert_eq!(count6::remeasure_differs(&json!({"edits": [{"x": 1}]})), None);
}

#[test]
fn count6_through_the_tally_gates_on_the_construction_and_reports_the_own_words_beside() {
    // the B23 pin's child: a span's first unit of 3 own words joining 6
    // parent words. Item 28.9's own-words reading counted it; its
    // constructed paragraph has 9 words, so amendment 31's gate does not.
    let p1 = format!("{} {}", prose(&SYN, 60, 1, 6, true), prose(&SYN, 61, 1, 4, true));
    let parent = format!("{p1}\n\n{}\n\n{}", prose(&SYN, 62, 3, 10, true), prose(&SYN, 63, 2, 10, true));
    let s = parent.find(&prose(&SYN, 61, 1, 4, true)).unwrap();
    let src = Source::Span { parent: &parent, span: (s, parent.len()) };
    let raw = format!("{} {} {}", prose(&ALT, 64, 1, 3, true), prose(&ALT, 65, 4, 10, true), prose(&ALT, 66, 2, 10, true));
    let conv = none_json();
    let l = engine::simple_verdict(Code::Landed, src, &conv, &raw).unwrap();
    let d = sha_hex(l.text.as_bytes());
    for (differs, gate) in [(Some(true), 0u64), (Some(false), 0), (None, 0)] {
        let mut t = Tally::default();
        let mut c = case(src, &raw, &conv, Recorded { accepted: l.accepted, reasons: &l.reasons, text_sha256: &d }, Ok((l.accepted, l.reasons.clone())), Ok((l.accepted, l.reasons.clone())));
        c.fin = Fin::Splice { parent: &parent, span: (s, parent.len()), doc_sha256: None };
        c.remeasure_differs = differs;
        t.count(&c);
        assert_eq!(t.mismatched(), 0, "{:?}", t.mismatches);
        let j = t.to_json();
        let g = &j["gate"]["count6_placed_break_paragraph_under_8_words"];
        assert_eq!((g["gate"].as_u64(), g["own_words_reading"].as_u64(), g["constructed_reading"].as_u64()), (Some(gate), Some(1), Some(0)), "{g}");
        assert_eq!(g["remeasured_short"].as_u64(), Some(0), "{g}");
        let cell = &t.cells["local|model_only|web"];
        assert_eq!(cell.get("c6_constructed_alone_readings").copied(), cell.get("c6_placed_paragraphs").copied(), "{cell:?}");
        assert!(t.ids.iter().all(|x| x["what"] != "count6_placed_break_paragraph_under_8_words"));
    }
}

// ------------------------------------------------------------------ B27 review: pins for the readings beside count 6 (finding 5)

#[test]
fn count6_an_insertion_alone_with_a_leading_line_break_is_read_from_its_lead() {
    // An insertion at the end of a parent paragraph laid out with item
    // 29.1's no-room line break and lead: the replacement's range in the
    // document starts at its line break, so its units sit where they are.
    // Read from the text after the lead instead, its last unit would reach
    // into the parent's next paragraph of 3 words.
    let p1 = prose(&SYN, 7, 1, 8, true);
    let parent = format!("{p1}\n\n{}", prose(&SYN, 8, 1, 3, true));
    let at = p1.len();
    let (src, mut t) = hand(&[(FIVE, B, false, Some(0)), (TEN, B, false, Some(0))], None, true);
    let lead = "\n  ";
    assert_eq!(grammar_a_form::profile::replacement_lead(&format!("{lead}{}", t.text)), Some(lead.len()));
    t.text = format!("{lead}{}", t.text);
    for p in t.paragraphs.iter_mut() {
        *p = (p.0 + lead.len(), p.1 + lead.len());
    }
    let c = count6::count6(&src, &t, Some(Own { parent: &parent, span: (at, at), sep: Some(" ") }), None);
    assert!(c.inconsistent.is_none());
    assert_eq!(c.not_one_paragraph, 0, "{c:?}");
    // the first unit joins the parent paragraph before it (8 + 5); the last is its own paragraph
    assert_eq!(c.units.iter().map(|x| (x.placed, x.alone, x.short)).collect::<Vec<_>>(), vec![(true, Some(13), false), (true, Some(10), false)]);
}

#[test]
fn count6_a_unit_in_several_constructed_paragraphs_takes_the_fewest_words_and_is_counted() {
    // A document whose construction cuts the child's first unit (another
    // placed edit's separator holding a line break inside it): the unit
    // overlaps a paragraph of 9 words and one of 3, takes 3 and is counted
    // in not_one_paragraph.
    let (parent, span) = span_parent(7);
    let (src, t) = hand_span(&parent, span, &[(FIVE, Some(0)), (TEN, Some(0))]);
    let doc = grammar_a_form::profile::splice(&parent, span, &t.text).unwrap();
    let fin = (span.0, span.0 + t.text.len());
    let m = fin.0 + "Acorn bramble ".len();
    let edits = vec![
        ItemEdit { span, sep: None, fin, units: t.paragraphs.clone() },
        ItemEdit { span: (parent.len(), parent.len()), sep: Some("\n".into()), fin: (m, m), units: Vec::new() },
    ];
    let own = Own { parent: &parent, span, sep: None };
    let c = count6::count6(&src, &t, Some(own), Some(ItemDoc { parent: &parent, doc: &doc, edits: &edits, index: 0 }));
    assert!(c.inconsistent.is_none(), "{c:?}");
    assert_eq!(c.not_one_paragraph, 1, "{c:?}");
    let u0 = &c.units[0];
    assert_eq!((u0.item, u0.alone, u0.short), (Some(3), Some(12), true), "the fewest of 9 and 3; alone 7 + 5: {u0:?}");
}

/// The B23 pin child (a span's first unit of 3 own words after 6 parent
/// words) and an item in which a sibling replaces the last 5 of those words
/// with two units, its last of 2 words: the child's first unit is in a constructed
/// paragraph of 5 words in the item, 9 alone and by pre_words.
#[test]
fn count6_through_the_tally_writes_the_construction_differs_ids_and_gates_on_the_item_reading() {
    let p1a = prose(&SYN, 60, 1, 6, true);
    let p1 = format!("{p1a} {}", prose(&SYN, 61, 1, 4, true));
    let parent = format!("{p1}\n\n{}\n\n{}", prose(&SYN, 62, 3, 10, true), prose(&SYN, 63, 2, 10, true));
    let s = parent.find(&prose(&SYN, 61, 1, 4, true)).unwrap();
    let span = (s, parent.len());
    let src = Source::Span { parent: &parent, span };
    let raw = format!("{} {} {}", prose(&ALT, 64, 1, 3, true), prose(&ALT, 65, 4, 10, true), prose(&ALT, 66, 2, 10, true));
    let conv = none_json();
    let (_, lt) = engine::landed_transfer(src, &conv, &raw).unwrap();
    let l = engine::simple_verdict(Code::Landed, src, &conv, &raw).unwrap();
    assert_eq!(l.text, lt.text);
    let sib = format!("{TEN}\n\nQuarry ridge.");
    // the sibling replaces all but the parent paragraph's first word (a human word stays)
    let sib_span = (p1a.find(' ').unwrap() + 1, p1a.len());
    let parts = [grammar_a_form::profile::Part { span: sib_span, insert_sep: None }, grammar_a_form::profile::Part { span, insert_sep: None }];
    let a = grammar_a_form::profile::assemble_item(&parent, &parts, &[&sib, &lt.text]).unwrap();
    let edits = vec![
        ItemEdit { span: sib_span, sep: None, fin: a.edit_spans[0], units: vec![(0, TEN.len()), (TEN.len() + 2, sib.len())] },
        ItemEdit { span, sep: None, fin: a.edit_spans[1], units: lt.paragraphs.clone() },
    ];
    let d = sha_hex(l.text.as_bytes());
    let mut t = Tally::default();
    let mut c = case(src, &raw, &conv, Recorded { accepted: l.accepted, reasons: &l.reasons, text_sha256: &d }, Ok((l.accepted, l.reasons.clone())), Ok((l.accepted, l.reasons.clone())));
    c.fin = Fin::Doc(&a.text, a.edit_spans[1]);
    c.item = Some(ItemDoc { parent: &parent, doc: &a.text, edits: &edits, index: 1 });
    t.count(&c);
    assert_eq!(t.mismatched(), 0, "{:?}", t.mismatches);
    let cell = &t.cells["local|model_only|web"];
    let g = |k: &str| cell.get(k).copied().unwrap_or(0);
    assert_eq!((g("c6_gate"), g("c6_constructed_item_short"), g("c6_constructed_alone_short"), g("c6_construction_differs")), (1, 1, 0, 1), "{cell:?}");
    let what: Vec<&str> = t.ids.iter().map(|x| x["what"].as_str().unwrap()).collect();
    assert!(what.contains(&"count6_placed_break_paragraph_under_8_words"), "{what:?}");
    let differs = t.ids.iter().find(|x| x["what"] == "count6_construction_differs").expect("item 31.1: its ids go to the ids-only file");
    assert_eq!(differs["kind"], "mixed");
    assert!(!serde_json::to_string(&t.ids).unwrap().contains("Quarry"), "ids only");
}

#[test]
fn count6_through_the_tally_reports_a_short_body_unit_at_a_fixed_boundary_and_does_not_gate_it() {
    // a whole text of one 5-word body unit: no placed break on either edge
    let parent = format!("{}\n\n{}", prose(&SYN, 5, 3, 9, true), prose(&SYN, 6, 3, 9, true));
    let src = Source::Mirror { parent: &parent };
    let raw = prose(&ALT, 9, 1, 5, true);
    let conv = none_json();
    let l = engine::simple_verdict(Code::Landed, src, &conv, &raw).unwrap();
    let d = sha_hex(l.text.as_bytes());
    let mut t = Tally::default();
    t.count(&case(src, &raw, &conv, Recorded { accepted: l.accepted, reasons: &l.reasons, text_sha256: &d }, Ok((l.accepted, l.reasons.clone())), Ok((l.accepted, l.reasons.clone()))));
    assert_eq!(t.mismatched(), 0, "{:?}", t.mismatches);
    let j = t.to_json();
    let c6 = &j["gate"]["count6_placed_break_paragraph_under_8_words"];
    assert_eq!((c6["fixed_boundary_short"].as_u64(), c6["gate"].as_u64()), (Some(1), Some(0)), "{c6}");
    assert_eq!(t.cells["local|model_only|web"].get("c6_fixed_boundary_units").copied(), Some(1));
}
