//! Protocol A2 amendment 29 item 29.1 for `cc_cohort_profile_v2`, on
//! synthetic text only (no corpus text, no live call): the edits of one
//! item are laid out in parent order, a later edit's first-line column and
//! an earlier edit's tail measured on the item assembled so far, a later
//! edit with no room on the shared line starting on a new line with a
//! continuation lead (`sibling_column_wraps`); one-edit items and edits on
//! lines of their own lay out exactly as each alone.
#[path = "support/a10_gen.rs"]
mod synth;

use grammar_a_form::conventions::{Choice, Conventions};
use grammar_a_form::measure::Measure;
use grammar_a_form::profile::{EditV2, Part, Slot, assemble_item, child_v2, edit_in_item_v2, item_v2, lay_out_item, replacement_lead};
use grammar_a_form::transfer::{SourceLayout, transfer, wrap};
use synth::Lcg;

fn none() -> Conventions {
    fn c<K>() -> Choice<K> {
        Choice { value: None, source: "none" }
    }
    Conventions { double: c(), single: c(), dash: c(), range: c(), ellipsis: c() }
}

fn cols(l: &str) -> usize {
    l.trim_end().chars().count()
}

/// The document lines wider than `w` that hold more than one word.
fn over(doc: &str, w: usize) -> Vec<usize> {
    doc.split('\n').enumerate().filter(|(_, l)| cols(l) > w && l.split_whitespace().count() > 1).map(|(i, _)| i).collect()
}

/// A hard-wrapped parent: `paras` paragraphs of 5 to 7 sentences,
/// greedy-wrapped at `w`. Returns the text and each paragraph's sentences'
/// byte spans.
fn parent(r: &mut Lcg, w: usize, paras: usize) -> (String, Vec<Vec<(usize, usize)>>) {
    parent_indented(r, w, paras, 0)
}

/// `parent` with each paragraph's first line indented by `indent` spaces
/// (its budget `w` less the indent) and its continuation lines flush: the
/// paragraph-first and continuation lead pools differ.
fn parent_indented(r: &mut Lcg, w: usize, paras: usize, indent: usize) -> (String, Vec<Vec<(usize, usize)>>) {
    let mut out = String::new();
    let mut spans = Vec::new();
    for p in 0..paras {
        if p > 0 {
            out.push_str("\n\n");
        }
        let n = 5 + r.below(3) as usize;
        let sents: Vec<String> = (0..n).map(|_| synth::sentence(r, true)).collect();
        let lines = wrap(&sents.join(" "), w, indent);
        let start = out.len();
        out.push_str(&" ".repeat(indent));
        out.push_str(&lines.join("\n"));
        // sentence spans: the text is the sentences joined by single spaces
        // or by the wrap's newlines, in order
        let mut ss = Vec::new();
        let mut at = start;
        for s in &sents {
            let words: Vec<&str> = s.split(' ').collect();
            let a = at + out[at..].find(words[0]).unwrap();
            let mut e = a;
            for wd in &words {
                e += out[e..].find(wd).unwrap() + wd.len();
            }
            ss.push((a, e));
            at = e;
        }
        spans.push(ss);
    }
    (out, spans)
}

/// The edits laid out alone (each as the only edit of its item: the layout
/// before amendment 29) and assembled, with each edit's final span.
fn alone(parent: &str, edits: &[EditV2]) -> Option<(String, Vec<(usize, usize)>)> {
    let reps: Vec<String> = edits.iter().map(|e| item_v2(parent, &none(), &[*e]).reps.into_iter().next().unwrap_or_default()).collect();
    let parts: Vec<Part> = edits.iter().map(|e| Part { span: e.span, insert_sep: e.insert_sep }).collect();
    let rs: Vec<&str> = reps.iter().map(String::as_str).collect();
    assemble_item(parent, &parts, &rs).ok().map(|a| (a.text, a.edit_spans))
}

/// The document lines over W that item 29.1 leaves as they are, both of
/// them one-word pieces of an edit beside parent text (amendment 29 item
/// 29.2: count 4 counts them):
/// * an edit's first word laid out after the parent's column (no earlier
///   edit on the line) where that word does not fit (item 10.1: a word
///   longer than its budget stands alone);
/// * an edit's last line of one word followed by parent text, when that word
///   and the text do not fit (item 13.7: the moved suffix holds at least one
///   word).
///
/// `edits`: each edit's final span and whether its column holds an earlier
/// edit's text.
fn excused(doc: &str, w: usize, edits: &[((usize, usize), bool)]) -> Vec<usize> {
    let mut out = Vec::new();
    for &((at, end), sibling) in edits {
        let ls = doc[..at].rfind('\n').map_or(0, |k| k + 1);
        let first = doc[at..].split_whitespace().next().unwrap_or("");
        if !sibling && ls < at && doc[ls..at].chars().count() + first.chars().count() > w {
            out.push(doc[..at].matches('\n').count());
        }
        let ll = doc[..end].rfind('\n').map_or(0, |k| k + 1).max(at);
        let le = doc[end..].find('\n').map_or(doc.len(), |k| end + k);
        if doc[ll..end].split_whitespace().count() == 1 && !doc[end..le].trim().is_empty() {
            out.push(doc[..end].matches('\n').count());
        }
    }
    out
}

/// `over` less `excused`.
fn over_not_excused(doc: &str, w: usize, edits: &[((usize, usize), bool)]) -> Vec<usize> {
    let ex = excused(doc, w, edits);
    over(doc, w).into_iter().filter(|l| !ex.contains(l)).collect()
}

/// A case: two edits in one paragraph whose shared parent line holds the
/// end of the first, parent text and the start of the second.
struct Shared {
    parent: String,
    w: usize,
    edits: Vec<(usize, usize, Option<&'static str>, String)>,
}

impl Shared {
    fn edits(&self) -> Vec<EditV2<'_>> {
        self.edits.iter().map(|(s, e, sep, raw)| EditV2 { span: (*s, *e), insert_sep: *sep, raw }).collect()
    }
}

/// Cases from seed `seed`: `kind` 0 two spans, 1 a span then an insertion,
/// 2 an insertion then a span. Each span replaces one or two sentences (the
/// first ends on the shared line, the second starts on it).
fn shared_cases(seed: u64, n: usize, w: usize, kind: u64) -> Vec<Shared> {
    shared_cases_in(seed, n, w, kind, 0)
}

/// `shared_cases` in parents whose paragraphs' first lines are indented by
/// `indent` spaces (`parent_indented`).
fn shared_cases_in(seed: u64, n: usize, w: usize, kind: u64, indent: usize) -> Vec<Shared> {
    let mut r = Lcg(seed);
    let mut out = Vec::new();
    let mut tries = 0;
    while out.len() < n && tries < 50 * n {
        tries += 1;
        let (p, spans) = parent_indented(&mut r, w, 2, indent);
        // W is the parent's measured width (its widest wrapped line)
        let Some(wm) = Measure::of(&p).counts().width.filter(|&wm| over(&p, wm).is_empty()) else { continue };
        let ss = &spans[r.below(2) as usize];
        // sentences a and b (b > a + 1) with a's end and b's start on one line
        let pairs: Vec<(usize, usize)> = (0..ss.len()).flat_map(|a| (a + 2..ss.len()).map(move |b| (a, b))).filter(|&(a, b)| !p[ss[a].1..ss[b].0].contains('\n')).collect();
        if pairs.is_empty() {
            continue;
        }
        let (a, b) = pairs[r.below(pairs.len() as u64) as usize];
        let first = (ss[a.saturating_sub(r.below(2) as usize)].0, ss[a].1);
        let second = (ss[b].0, ss[(b + r.below(2) as usize).min(ss.len() - 1)].1);
        let mut raw = || (0..1 + r.below(3)).map(|_| synth::sentence(&mut r, true)).collect::<Vec<_>>().join(" ");
        let edits = match kind {
            0 => vec![(first.0, first.1, None, raw()), (second.0, second.1, None, raw())],
            // an insertion after the sentence before b, on the shared line
            1 => vec![(first.0, first.1, None, raw()), (ss[b - 1].1, ss[b - 1].1, Some(" "), raw())],
            _ => vec![(ss[a].1, ss[a].1, Some(" "), raw()), (second.0, second.1, None, raw())],
        };
        out.push(Shared { parent: p, w: wm, edits });
    }
    out
}

/// Runs a batch of cases: (cases, over W before amendment 29, over W now,
/// no-room wraps). Every case must assemble now, and every later edit laid
/// out with its earlier sibling known (`edit_in_item_v2`) must be the item's.
fn run(cases: &[Shared]) -> (usize, usize, usize, usize) {
    let (mut n, mut before, mut now, mut wraps) = (0, 0, 0, 0);
    for c in cases {
        assert_eq!(Measure::of(&c.parent).counts().width, Some(c.w), "the parent is hard-wrapped at W");
        let edits = c.edits();
        let it = item_v2(&c.parent, &none(), &edits);
        let Some(a) = &it.assembled else { panic!("{:?}", it.reasons) };
        n += 1;
        if alone(&c.parent, &edits).is_some_and(|(d, sp)| !over_not_excused(&d, c.w, &sp.iter().map(|x| (*x, false)).collect::<Vec<_>>()).is_empty()) {
            before += 1;
        }
        let slots: Vec<Slot> = edits.iter().map(|e| Slot { span: e.span, insert_sep: e.insert_sep, raw: Some(e.raw) }).collect();
        let laid = lay_out_item(&c.parent, &none(), &slots);
        let firsts: Vec<((usize, usize), bool)> = (0..edits.len()).map(|k| (a.edit_spans[k], laid[k].as_ref().unwrap().src.sibling_column)).collect();
        let o = over_not_excused(&a.text, c.w, &firsts);
        // the order of the edits as given does not matter: assembly's order is
        let rev: Vec<EditV2> = edits.iter().rev().copied().collect();
        let back = item_v2(&c.parent, &none(), &rev);
        assert_eq!(back.reps.iter().rev().collect::<Vec<_>>(), it.reps.iter().collect::<Vec<_>>());
        if !o.is_empty() {
            now += 1;
        }
        let k: usize = it.record["edits"].as_array().unwrap().iter().map(|e| e["transfer"]["sibling_column_wraps"].as_u64().unwrap_or(0) as usize).sum();
        wraps += k;
        // the earlier request's text, judged first with its sibling pending,
        // and the later one's, judged with its earlier sibling known, are
        // the item check's
        let order: Vec<usize> = {
            let mut o: Vec<usize> = (0..2).collect();
            o.sort_by_key(|&i| edits[i].span);
            o
        };
        let mut first = slots.clone();
        first[order[1]].raw = None;
        assert_eq!(edit_in_item_v2(&c.parent, &none(), &first, order[0]).reps[0], it.reps[order[0]]);
        assert_eq!(edit_in_item_v2(&c.parent, &none(), &slots, order[1]).reps[0], it.reps[order[1]]);
        // with the earlier sibling pending, the later edit is laid out as alone
        let mut pend = slots.clone();
        pend[order[0]].raw = None;
        assert_eq!(edit_in_item_v2(&c.parent, &none(), &pend, order[1]).reps[0], item_v2(&c.parent, &none(), &[edits[order[1]]]).reps[0]);
    }
    (n, before, now, wraps)
}

// ------------------------------------------------------------------ item 29.1

#[test]
fn two_spans_sharing_a_line_fit_on_the_assembled_line() {
    // Each edit laid out alone fits its own budgets, but the assembled
    // shared line can be wider than W (count 4's 3 documents, amendment 29's
    // trigger). Laid out in parent order, no document line of several words
    // is wider than W.
    for (seed, w) in [(2901u64, 72usize), (2902, 75), (2903, 60)] {
        let cases = shared_cases(seed, 120, w, 0);
        let (n, before, now, wraps) = run(&cases);
        assert!(n == cases.len() && n >= 100, "{n}");
        assert!(before >= 3, "W {w}: the layout before amendment 29 puts shared lines over W ({before})");
        assert_eq!(now, 0, "W {w}");
        assert!(wraps >= 1, "W {w}: some later edit has no room on its shared line ({wraps})");
    }
}

#[test]
fn an_insertion_and_a_span_sharing_a_line_fit() {
    for kind in [1u64, 2] {
        let cases = shared_cases(2910 + kind, 120, 72, kind);
        let (n, before, now, _) = run(&cases);
        assert!(n == cases.len() && n >= 100, "{n}");
        assert!(before >= 1, "kind {kind}: {before}");
        assert_eq!(now, 0, "kind {kind}");
    }
}

/// The structure of count 4's three documents (amendment 29's trigger): a
/// spliced item in a hard-wrapped
/// parent whose widest line is W, an earlier edit spanning parent lines and
/// ending on a line that then holds parent text and a later edit's first
/// line, which runs on over more lines. Each piece fits on its own; laid
/// out alone, the document line is wider than W (two at W 72, one at W 75,
/// the earlier piece of 1, 3 and 2 words and the later of 3, 3 and 4).
#[test]
fn count_4s_three_documents_structurally() {
    let want = [(75usize, 1usize, 3usize), (72, 3, 3), (72, 2, 4)];
    for (w, ew, lw) in want {
        let mut found = None;
        for seed in 0..400u64 {
            for c in shared_cases(29_000 + seed * 7 + w as u64, 4, w, 0) {
                if c.w != w {
                    continue;
                }
                let edits = c.edits();
                let Some((old, _)) = alone(&c.parent, &edits) else { continue };
                let lines: Vec<&str> = old.split('\n').collect();
                let reps: Vec<String> = edits.iter().map(|e| item_v2(&c.parent, &none(), &[*e]).reps[0].clone()).collect();
                let (r0, r1) = (&reps[0], &reps[1]);
                if r0.lines().count() < 2 || r1.lines().count() < 2 {
                    continue;
                }
                let (last0, first1) = (r0.lines().last().unwrap(), r1.lines().next().unwrap());
                let shape = lines.iter().any(|l| cols(l) > w && l.starts_with(last0) && l.trim_end().ends_with(first1));
                if shape && last0.split_whitespace().count() == ew && first1.split_whitespace().count() == lw {
                    found = Some(c);
                    break;
                }
            }
            if found.is_some() {
                break;
            }
        }
        let c = found.unwrap_or_else(|| panic!("no case of shape W {w}, {ew} and {lw} words"));
        let it = item_v2(&c.parent, &none(), &c.edits());
        let a = it.assembled.as_ref().expect("assembles");
        let slots: Vec<Slot> = c.edits().iter().map(|e| Slot { span: e.span, insert_sep: e.insert_sep, raw: Some(e.raw) }).collect();
        let laid = lay_out_item(&c.parent, &none(), &slots);
        let firsts: Vec<((usize, usize), bool)> = (0..2).map(|k| (a.edit_spans[k], laid[k].as_ref().unwrap().src.sibling_column)).collect();
        assert!(over_not_excused(&a.text, w, &firsts).is_empty(), "W {w}: the shared line now fits");
    }
}

#[test]
fn a_later_edits_column_and_an_earlier_edits_tail_come_from_the_assembled_line() {
    let cases = shared_cases(2920, 60, 72, 0);
    let mut checked = 0;
    for c in &cases {
        let edits = c.edits();
        let slots: Vec<Slot> = edits.iter().map(|e| Slot { span: e.span, insert_sep: e.insert_sep, raw: Some(e.raw) }).collect();
        let laid = lay_out_item(&c.parent, &none(), &slots);
        let (l0, l1) = (laid[0].as_ref().unwrap(), laid[1].as_ref().unwrap());
        // the earlier edit's tail is the parent text between the two edits
        assert_eq!(l0.src.tail_column, c.parent[edits[0].span.1..edits[1].span.0].chars().count());
        assert!(!l0.src.sibling_column);
        // the later edit's column is the document line's text before it
        let it = item_v2(&c.parent, &none(), &edits);
        let a = it.assembled.unwrap();
        let s1 = a.edit_spans[1].0 - replacement_lead(&it.reps[1]).unwrap();
        let line_start = a.text[..s1].rfind('\n').map_or(0, |k| k + 1);
        assert_eq!(l1.src.column, a.text[line_start..s1].chars().count());
        assert!(l1.src.sibling_column);
        // the parent's own measures are unchanged
        let alone1 = SourceLayout::span(&c.parent, edits[1].span.0, edits[1].span.1).unwrap();
        assert_eq!((l1.src.structure.clone(), l1.src.licence.clone(), l1.src.width), (alone1.structure, alone1.licence, alone1.width));
        checked += 1;
    }
    assert_eq!(checked, 60);
}

#[test]
fn with_no_room_the_later_edit_starts_a_new_line_with_a_continuation_lead() {
    // Item 29.1's zero-room case, read on its records: the replacement
    // begins with one line break and a continuation lead, assembly accepts
    // it, and the edit's final span starts after the lead.
    let mut seen = 0;
    for seed in 0..40u64 {
        for c in shared_cases(2930 + seed, 10, 72, 0) {
            let edits = c.edits();
            let it = item_v2(&c.parent, &none(), &edits);
            for (k, e) in it.record["edits"].as_array().unwrap().iter().enumerate() {
                if e["transfer"]["sibling_column_wraps"] != 1 {
                    assert!(e["transfer"].get("sibling_column_wraps").is_none(), "recorded only when not 0");
                    continue;
                }
                seen += 1;
                let rep = &it.reps[k];
                assert!(rep.starts_with('\n') && !rep.trim_start().is_empty(), "{rep:?}");
                let lead = replacement_lead(rep).unwrap();
                assert!(!rep[1..lead].contains('\n'));
                let a = it.assembled.as_ref().expect("assembles");
                let (s, e2) = a.edit_spans[k];
                assert_eq!(&a.text[s..e2], rep.trim_start());
                // the line before the break holds the earlier edit's last
                // line and the parent text: within W, and the first word
                // would not have fitted
                let before = &a.text[..s - lead];
                let line = &before[before.rfind('\n').map_or(0, |x| x + 1)..];
                assert!(cols(line) <= c.w);
                let first = rep.split_whitespace().next().unwrap();
                assert!(line.chars().count() + first.chars().count() > c.w, "{} + {}", line.chars().count(), first.chars().count());
                // the new line's budget is W less its lead: greedy, so the
                // next line's first word would not have fitted on it (unless
                // that line is item 13.7's moved last line)
                let lines: Vec<&str> = rep[1..].split('\n').collect();
                if lines.len() > 2 || (lines.len() == 2 && e["transfer"]["tail_budget_splits"] == 0) {
                    let next = lines[1].split_whitespace().next().unwrap();
                    assert!(cols(lines[0]) <= c.w && cols(lines[0]) + 1 + next.chars().count() > c.w, "{lines:?}");
                }
                assert!(over(line, c.w).is_empty());
            }
        }
    }
    assert!(seen >= 3, "{seen}");
}

#[test]
fn one_edit_items_and_edits_on_their_own_lines_lay_out_as_alone() {
    // A one-edit item is laid out exactly as before (its own source,
    // parent columns), and so is every edit of an item that shares no line
    // with another edit.
    let mut r = Lcg(2940);
    let mut items = 0;
    for _ in 0..60 {
        let (p, spans) = parent(&mut r, 66, 3);
        let s0 = spans[0][1];
        let raw = synth::sentence(&mut r, true);
        let one = item_v2(&p, &none(), &[EditV2 { span: s0, insert_sep: None, raw: &raw }]);
        let ch = child_v2(&p, Some(s0), &none(), &raw);
        assert_eq!(one.reps[0], ch.text);
        assert_eq!(one.reps[0], transfer(&raw, &none(), &SourceLayout::span(&p, s0.0, s0.1).unwrap()).text);
        assert_eq!(one.record["edits"][0]["transfer"], ch.record["transfer"]);
        // two edits in different paragraphs share no line
        let s2 = spans[2][2];
        let raw2 = synth::sentence(&mut r, true);
        let two = item_v2(&p, &none(), &[EditV2 { span: s0, insert_sep: None, raw: &raw }, EditV2 { span: s2, insert_sep: None, raw: &raw2 }]);
        assert_eq!(two.reps[0], one.reps[0]);
        assert_eq!(two.reps[1], item_v2(&p, &none(), &[EditV2 { span: s2, insert_sep: None, raw: &raw2 }]).reps[0]);
        items += 1;
    }
    assert_eq!(items, 60);
    // an unwrapped parent has no budget: a shared line changes nothing
    let p = "Alpha one two three four five six. Beta one two three four five six. Gamma one two three four five six.";
    let (b, g) = (p.find("Beta").unwrap(), p.find("Gamma").unwrap());
    let e = [EditV2 { span: (0, b - 1), insert_sep: None, raw: "The first sentence was rewritten here today." }, EditV2 { span: (g, p.len()), insert_sep: None, raw: "The third one was too." }];
    let it = item_v2(p, &none(), &e);
    assert_eq!(it.reps[1], item_v2(p, &none(), &e[1..]).reps[0]);
}

#[test]
fn assembly_accepts_only_a_leading_line_break_and_lead() {
    let p = "One two three. Four five six. Seven eight nine.";
    let (a, b) = (p.find("Four").unwrap(), p.find("six.").unwrap() + 4);
    let part = [Part { span: (a, b), insert_sep: None }];
    let ok = assemble_item(p, &part, &["\n  Ten eleven twelve."]).unwrap();
    assert_eq!(&ok.text[ok.edit_spans[0].0..ok.edit_spans[0].1], "Ten eleven twelve.");
    assert_eq!(ok.text, "One two three. \n  Ten eleven twelve. Seven eight nine.");
    for bad in ["  Ten eleven.", "\n\nTen eleven.", "\n \nTen eleven.", "Ten eleven. ", "\n  ", "\n"] {
        assert_eq!(assemble_item(p, &part, &[bad]).unwrap_err(), "replacement layout", "{bad:?}");
    }
    assert_eq!(replacement_lead("x"), Some(0));
    assert_eq!(replacement_lead("\n\tx"), Some(2));
    assert_eq!(replacement_lead(" x"), None);
}

#[test]
fn the_no_room_line_takes_a_continuation_lead_not_a_paragraph_first_lead() {
    // Item 29.1's zero-room case takes a continuation lead (item 10.6(c)):
    // in a parent whose paragraphs' first lines are indented and whose
    // continuation lines are flush, the line break is followed by the flush
    // continuation lead, never the first-line indent. The later edit runs
    // into the next paragraph, so its source holds both pools.
    let mut r = Lcg(2950);
    let mut seen = 0;
    for _ in 0..3000 {
        if seen >= 5 {
            break;
        }
        let (p, spans) = parent_indented(&mut r, 72, 2, 4);
        if Measure::of(&p).counts().width.filter(|&wm| over(&p, wm).is_empty()).is_none() {
            continue;
        }
        let ss = &spans[0];
        let pairs: Vec<(usize, usize)> = (0..ss.len()).flat_map(|a| (a + 2..ss.len()).map(move |b| (a, b))).filter(|&(a, b)| !p[ss[a].1..ss[b].0].contains('\n')).collect();
        if pairs.is_empty() {
            continue;
        }
        let (a, b) = pairs[r.below(pairs.len() as u64) as usize];
        let raw1 = synth::sentence(&mut r, true);
        let raw2 = (0..2 + r.below(2)).map(|_| synth::sentence(&mut r, true)).collect::<Vec<_>>().join(" ");
        let edits = [EditV2 { span: ss[a], insert_sep: None, raw: &raw1 }, EditV2 { span: (ss[b].0, spans[1][0].1), insert_sep: None, raw: &raw2 }];
        let it = item_v2(&p, &none(), &edits);
        if it.record["edits"][1]["transfer"]["sibling_column_wraps"] != 1 {
            continue;
        }
        let slots: Vec<Slot> = edits.iter().map(|e| Slot { span: e.span, insert_sep: e.insert_sep, raw: Some(e.raw) }).collect();
        let laid = lay_out_item(&p, &none(), &slots);
        let src = &laid[1].as_ref().unwrap().src;
        // the source's two pools differ: the indent is a paragraph-first lead only
        assert!(src.pools.first_leads.iter().any(|l| l == "    "), "{:?}", src.pools.first_leads);
        assert!(!src.pools.cont_leads.is_empty() && src.pools.cont_leads.iter().all(String::is_empty), "{:?}", src.pools.cont_leads);
        let rep = &it.reps[1];
        assert_eq!(replacement_lead(rep), Some(1), "{rep:?}: one line break and the flush continuation lead");
        seen += 1;
    }
    assert!(seen >= 5, "{seen}");
}

#[test]
fn a_later_edit_on_a_line_of_its_own_keeps_the_parents_column_and_a_long_first_word_stands_alone() {
    // Item 29.1's "Unchanged": a later edit whose document line holds no
    // earlier edit's text keeps the parent's column, takes no no-room line
    // break, and lays out exactly as alone; item 10.1's long first word,
    // wider than W less that column, stands alone after the parent text.
    let long = "Incomprehensibilities";
    let mut r = Lcg(2960);
    let mut seen = 0;
    for _ in 0..300 {
        let (p, spans) = parent(&mut r, 72, 2);
        let Some(w) = Measure::of(&p).counts().width.filter(|&wm| over(&p, wm).is_empty()) else { continue };
        let first = spans[0][0];
        let later = spans[0].iter().skip(1).chain(spans[1].iter()).copied().find(|&(s, _)| {
            let ls = p[..s].rfind('\n').map_or(0, |k| k + 1);
            p[first.1..s].contains('\n') && ls < s && p[ls..s].chars().count() + long.len() > w
        });
        let Some((s, e)) = later else { continue };
        let col = p[p[..s].rfind('\n').map_or(0, |k| k + 1)..s].chars().count();
        let raw1 = synth::sentence(&mut r, true);
        let raw2 = format!("{long} {}", synth::sentence(&mut r, true));
        let edits = [EditV2 { span: first, insert_sep: None, raw: &raw1 }, EditV2 { span: (s, e), insert_sep: None, raw: &raw2 }];
        let it = item_v2(&p, &none(), &edits);
        let slots: Vec<Slot> = edits.iter().map(|e| Slot { span: e.span, insert_sep: e.insert_sep, raw: Some(e.raw) }).collect();
        let laid = lay_out_item(&p, &none(), &slots);
        let l1 = laid[1].as_ref().unwrap();
        assert!(!l1.src.sibling_column);
        assert_eq!(l1.src.column, col);
        let rep = &it.reps[1];
        assert!(rep.starts_with(long), "{rep:?}: no leading line break; the long word stands alone at the parent's column");
        assert!(it.record["edits"][1]["transfer"].get("sibling_column_wraps").is_none());
        assert_eq!(*rep, item_v2(&p, &none(), &edits[1..]).reps[0]);
        assert_eq!(it.reps[0], item_v2(&p, &none(), &edits[..1]).reps[0]);
        seen += 1;
    }
    assert!(seen >= 10, "{seen}");
}

#[test]
fn an_earlier_edits_tail_before_an_insertion_whose_separator_breaks_the_line_is_the_parents() {
    // Item 29.1's tail, read on the text: a later insertion whose separator
    // holds a line break starts its text on a new line, so the next edit
    // does not start on the same document line, and the earlier edit's
    // tail counts the parent text up to the line's body end, as for the
    // edit alone ("Otherwise ... as now"). With a separator that holds no
    // line break, the tail is the parent text between the two edits.
    let mut checked = 0;
    for c in shared_cases(2970, 40, 72, 0) {
        let (s0, e0) = (c.edits[0].0, c.edits[0].1);
        let at = c.edits[1].0;
        let alone = SourceLayout::span(&c.parent, s0, e0).unwrap();
        let between = c.parent[e0..at].chars().count();
        assert!(!c.parent[e0..at].contains('\n') && alone.tail_column > between, "parent text follows the insertion point on the line");
        let raw0 = c.edits[0].3.as_str();
        for (sep, tail) in [("\n\n", alone.tail_column), ("\n", alone.tail_column), (" ", between)] {
            let slots = [Slot { span: (s0, e0), insert_sep: None, raw: Some(raw0) }, Slot { span: (at, at), insert_sep: Some(sep), raw: Some("Inserted words stand here.") }];
            let laid = lay_out_item(&c.parent, &none(), &slots);
            assert_eq!(laid[0].as_ref().unwrap().src.tail_column, tail, "{sep:?}");
        }
        checked += 1;
    }
    assert_eq!(checked, 40);
}
