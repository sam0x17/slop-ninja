//! Protocol A2 amendment 42 for `cc_cohort_profile_v2` (`layout_transfer_v6`),
//! on synthetic text only (no corpus text, no live call): the boundary break
//! after an edit's last word (item 42.1), its (a-ii) construction and the
//! edit's extended range (item 42.2), and item 13.7's one-word case (item
//! 42.3). The parents copy the shape of count 4's 74 lines (one child word
//! at the start of a document line, lead 0, followed by the parent's rest of
//! the line, which starts with 1 or 2 spaces, over W): hard-wrapped book
//! prose. The geometry of each break is pinned in `transfer`'s unit tests;
//! the assemblers' parity in `grammar-cc-human`.
#[path = "support/a10_gen.rs"]
mod synth;

use grammar_a_form::conventions::{Choice, Conventions};
use grammar_a_form::measure::{LayoutClass, Measure};
use grammar_a_form::profile::{EditV2, Part, Placed, Slot, assemble_item, child_v2, constructed_paragraphs, item_v2, lay_out_item, replacement_range, replacement_tail, splice};
use grammar_a_form::transfer::{SourceLayout, Transferred, transfer};
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

const LONG: [&str; 4] = ["Notwithstanding", "Characteristically", "Internationalisation", "Counterrevolutionaries"];

/// A hard-wrapped book parent (blank-line paragraphs, flush lines, W 60 to
/// 72) whose every line of several words is within W, with each sentence's
/// byte span; `gap` is the space between sentences (" " or "  "), put back
/// after wrapping where the line keeps room for it.
struct Parent {
    text: String,
    w: usize,
    sents: Vec<(usize, usize)>,
}

fn parent(r: &mut Lcg, gap: &str) -> Option<Parent> {
    let w = [60usize, 66, 72][r.below(3) as usize];
    let mut blocks = Vec::new();
    for _ in 0..3 {
        let n = 4 + r.below(3) as usize;
        let sents: Vec<String> = (0..n).map(|_| synth::sentence(r, true)).collect();
        let lines = grammar_a_form::transfer::wrap(&sents.join(" "), w, 0);
        let lines: Vec<String> = lines.into_iter().map(|l| if gap == " " || cols(&l) + 4 > w { l } else { l.replace(". ", &format!(".{gap}")) }).collect();
        blocks.push(lines.join("\n"));
    }
    let text = blocks.join("\n\n");
    let c = Measure::of(&text).counts();
    let w = c.width?;
    let fits = text.split('\n').all(|l| cols(l) <= w || l.split_whitespace().count() < 2);
    if c.class == LayoutClass::LineForm || !fits {
        return None;
    }
    let sents = grammar_a_form::text::sentences(&text).iter().map(|s| (s.start, s.end)).collect();
    Some(Parent { text, w, sents })
}

/// The columns of the parent text after byte `e` on its line.
fn tail_cols(p: &str, e: usize) -> usize {
    p[e..p[e..].find('\n').map_or(p.len(), |k| e + k)].trim_end().chars().count()
}

/// The transfer as amendment 38's split did not exist (`SourceLayout::line`
/// None): item 13.7 then splits as layout_transfer_v5 did (no boundary
/// break can be read), and no document line is split.
fn unsplit(raw: &str, src: &SourceLayout) -> Transferred {
    let mut s = src.clone();
    s.line = None;
    transfer(raw, &none(), &s)
}

/// A child of one paragraph ending with a long word.
fn raw_ending_long(r: &mut Lcg, k: usize) -> String {
    let s = (0..1 + r.below(2)).map(|_| synth::sentence(r, false)).collect::<Vec<_>>().join(" ");
    format!("{s} {}.", LONG[k % LONG.len()].to_lowercase())
}

/// Checks a single span edit's final document and its extended range: the
/// parent's bytes before the span and after its whitespace run are
/// unchanged, the replacement keeps that run, then one line break and the
/// lead, and the final span ends at the edit's last word.
fn check_document(p: &str, (s, e): (usize, usize), t: &Transferred) -> String {
    let doc = splice(p, (s, e), &t.text).unwrap();
    let tb = t.tail_break().expect("a boundary break");
    let (run, tail) = replacement_tail(&t.text).unwrap();
    assert_eq!((run, tail), (tb.run, tb.run + 1 + tb.lead));
    assert_eq!(&doc[..s], &p[..s]);
    assert_eq!(&doc[s..s + t.text.len()], t.text);
    assert_eq!(&t.text[t.text.len() - tail..][..run], &p[e..e + run], "the parent's run, byte for byte");
    assert_eq!(&doc[s + t.text.len()..], &p[e + run..], "the parent resumes at its first non-whitespace byte");
    assert!(!p[e + run..].starts_with(char::is_whitespace));
    // an item of one edit gives the same document and final span
    let a = assemble_item(p, &[Part { span: (s, e), insert_sep: None }], &[&t.text]).unwrap();
    assert_eq!(a.text, doc);
    let fin = a.edit_spans[0];
    assert!(!doc[fin.0..fin.1].ends_with(char::is_whitespace));
    assert_eq!(replacement_range(fin, &t.text), (s, s + t.text.len()));
    // the paragraphs by construction (amendment 13 item 13.1): the edit's
    // last unit joins the parent text after it up to the parent's boundary,
    // and every parent paragraph after that is the parent's, byte for byte
    let paras = constructed_paragraphs(p, &doc, &[Placed { span: (s, e), sep: None, fin: (s, s + t.text.len()), units: &t.paragraphs }]);
    let pm = Measure::of(p);
    let own = pm.paragraphs.iter().position(|x| x.range.0 <= e && e <= x.range.1).unwrap();
    let pe = pm.paragraphs[own].range.1;
    let joined = paras.iter().find(|x| x.0 < s + t.text.len() && s + t.text.len() <= x.1).expect("the tail joins the edit's paragraph");
    assert_eq!(&doc[s + t.text.len()..joined.1], p[e + run..pe].trim_end(), "up to the parent's boundary");
    let after: Vec<&str> = paras.iter().filter(|x| x.0 > joined.1).map(|&(a, b)| &doc[a..b]).collect();
    let want: Vec<&str> = pm.paragraphs[own + 1..].iter().map(|x| p[x.range.0..x.range.1].trim()).collect();
    assert_eq!(after, want);
    doc
}

#[derive(Default, Debug)]
struct Seen {
    room_above: usize,
    no_room_above: usize,
    blank_above: usize,
    wide_tail: usize,
    both_sides: usize,
    two_spaces: usize,
}

/// Single span edits whose child ends with a long word, so that its last
/// line and the parent's tail do not fit together.
fn sweep(seed: u64, gap: &str, n: usize) -> Seen {
    let mut r = Lcg(seed);
    let mut seen = Seen::default();
    let mut found = 0;
    for _ in 0..40 * n {
        if found >= n {
            break;
        }
        let Some(p) = parent(&mut r, gap) else { continue };
        let (s, e) = p.sents[r.below(p.sents.len() as u64) as usize];
        if tail_cols(&p.text, e) < 12 {
            continue;
        }
        let raw = raw_ending_long(&mut r, found);
        let src = SourceLayout::span(&p.text, s, e).unwrap();
        let new = transfer(&raw, &none(), &src);
        let old = unsplit(&raw, &src);
        let Some(tb) = new.tail_break() else {
            // no boundary break: the layout is v5's (the split took no
            // boundary, and item 13.7 split as before)
            assert_eq!(new.record.tail_budget_splits, old.record.tail_budget_splits);
            assert!(new.last_line_shares_parent_line());
            continue;
        };
        found += 1;
        assert_eq!(new.record.tail_boundary_breaks, 1);
        assert!(!new.last_line_shares_parent_line());
        seen.two_spaces += usize::from(tb.run == 2);
        let doc = check_document(&p.text, (s, e), &new);
        // the edit's last line before the break: within W, or one word
        let body_end = new.paragraphs.last().unwrap().1;
        let ls = doc[..s + body_end].rfind('\n').map_or(0, |k| k + 1);
        let last = &doc[ls..s + body_end];
        let one_word = last.split_whitespace().count() == 1;
        assert!(cols(last) <= p.w || one_word, "{last:?}");
        // the parent's tail line holds parent text only
        let tl = s + new.text.len();
        let tail_line = &doc[tl - tb.lead..doc[tl..].find('\n').map_or(doc.len(), |k| tl + k)];
        assert_eq!(tail_line.trim_start(), p.text[e + tb.run..p.text[e..].find('\n').map_or(p.text.len(), |k| e + k)].trim_start());
        seen.wide_tail += usize::from(cols(tail_line) > p.w);
        if new.text.starts_with('\n') {
            seen.both_sides += 1;
        }
        if old.record.tail_budget_splits > new.record.tail_budget_splits {
            // item 13.7's one-word case (item 42.3): v5 moved the last word
            // down beside the tail; now it stays on its line, which fits
            assert_eq!(old.record.tail_budget_splits, new.record.tail_budget_splits + 1);
            let old_lines: Vec<&str> = old.text.split('\n').collect();
            let moved = old_lines.last().unwrap();
            assert_eq!(moved.split_whitespace().count(), 1, "v5's fallback moved one word: {old_lines:?}");
            assert!(last.trim_end().ends_with(moved.trim()));
            assert!(!one_word);
            seen.room_above += 1;
        } else if one_word {
            // greedy wrap had already left the word alone: its line is
            // exempt, and the tail starts the next line
            let before = &doc[..ls];
            if before.ends_with("\n\n") || before.is_empty() {
                seen.blank_above += 1;
            } else {
                seen.no_room_above += 1;
            }
        }
    }
    assert_eq!(found, n, "{seen:?}");
    seen
}

#[test]
fn one_child_word_and_a_tail_over_w_with_and_without_room_above() {
    let s = sweep(4201, " ", 120);
    eprintln!("{s:?}");
    assert!(s.room_above >= 10 && s.no_room_above >= 3, "{s:?}");
}

#[test]
fn a_two_space_boundary_keeps_both_spaces_before_the_break() {
    let s = sweep(4202, "  ", 80);
    eprintln!("{s:?}");
    assert!(s.two_spaces >= 10, "{s:?}");
}

#[test]
fn a_start_side_and_an_end_side_break_on_one_edit() {
    // An edit starting late on its parent line, whose first word has no
    // room after the parent's column, and whose last line does not fit with
    // the tail: the replacement begins with the start-side break (item
    // 38.1's break before the first word) and ends with the boundary break.
    let mut r = Lcg(4203);
    let mut seen = 0;
    for _ in 0..4000 {
        let Some(p) = parent(&mut r, " ") else { continue };
        let Some(&(s, e)) = p.sents.iter().find(|&&(s, e)| {
            let ls = p.text[..s].rfind('\n').map_or(0, |k| k + 1);
            let col = p.text[ls..s].chars().count();
            !p.text[ls..s].trim().is_empty() && col + LONG[3].len() > p.w && tail_cols(&p.text, e) > 20
        }) else {
            continue;
        };
        // a first word with no room after the column, then words that wrap
        // so the last line and the tail are over W
        let raw = format!("{} {}", LONG[3], raw_ending_long(&mut r, 2));
        let src = SourceLayout::span(&p.text, s, e).unwrap();
        let t = transfer(&raw, &none(), &src);
        if !(t.text.starts_with('\n') && t.tail_break().is_some()) {
            continue;
        }
        let doc = check_document(&p.text, (s, e), &t);
        // the line before the start-side break keeps the parent's bytes
        assert_eq!(&doc[..s], &p.text[..s]);
        // every line holding the edit's words is within W or one word
        let fin = assemble_item(&p.text, &[Part { span: (s, e), insert_sep: None }], &[&t.text]).unwrap().edit_spans[0];
        for l in doc[fin.0..fin.1].split('\n') {
            assert!(cols(l) <= p.w || l.split_whitespace().count() == 1, "{l:?}");
        }
        // the rows' and the frontier's final span: from after the lead to
        // the last word
        assert_eq!(&doc[fin.0..fin.1], t.text.trim());
        seen += 1;
        if seen >= 10 {
            break;
        }
    }
    assert!(seen >= 10, "{seen}");
}

#[test]
fn an_insertion_at_the_end_of_a_line_is_unchanged() {
    // No parent word follows it on its line: no boundary break, no 13.7 tail,
    // and the record carries neither counter.
    let mut r = Lcg(4204);
    let mut seen = 0;
    for _ in 0..200 {
        let Some(p) = parent(&mut r, " ") else { continue };
        let ends: Vec<usize> = p.text.match_indices('\n').map(|(k, _)| k).filter(|&k| k > 0 && !p.text[..k].ends_with('\n')).collect();
        let at = ends[r.below(ends.len() as u64) as usize];
        let raw = raw_ending_long(&mut r, seen);
        let src = SourceLayout::insertion(&p.text, at, " ").unwrap();
        assert_eq!(src.tail_column, 0);
        let t = transfer(&raw, &none(), &src);
        assert!(t.tail_break().is_none());
        let rec = serde_json::to_value(&t.record).unwrap();
        assert!(rec.get("tail_boundary_breaks").is_none() && rec.get("tail_boundary_unavailable").is_none());
        let it = item_v2(&p.text, &none(), &[EditV2 { span: (at, at), insert_sep: Some(" "), raw: &raw }]);
        assert_eq!(it.reps[0], t.text);
        assert!(it.assembled.is_some(), "{:?}", it.reasons);
        seen += 1;
    }
    assert!(seen >= 50, "{seen}");
}

#[test]
fn an_insertion_glued_to_the_parent_text_has_no_boundary_break() {
    // An insertion before a parent word (no whitespace after its point):
    // item 42.1's insertion case. There is no boundary break; item 13.7
    // moves one word as before; the line is left and the edit is counted in
    // `tail_boundary_unavailable` when no inside break fits.
    let mut r = Lcg(4205);
    let (mut seen, mut counted) = (0, 0);
    for _ in 0..3000 {
        let Some(p) = parent(&mut r, " ") else { continue };
        // the start of a word early on a line, after a space, so the tail
        // glued to the insertion is long
        let lines: Vec<(usize, &str)> = p.text.split('\n').scan(0usize, |at, l| {
            let x = (*at, l);
            *at += l.len() + 1;
            Some(x)
        }).collect();
        let (ls, l) = lines[r.below(lines.len() as u64) as usize];
        let Some(k) = l.char_indices().filter(|&(i, c)| c == ' ' && i >= 3 && i < l.len() / 4).map(|(i, _)| i + 1).next() else { continue };
        let at = ls + k;
        let raw = format!("{} {}", synth::sentence(&mut r, false), LONG[3].to_lowercase());
        let src = SourceLayout::insertion(&p.text, at, " ").unwrap();
        let t = transfer(&raw, &none(), &src);
        let old = unsplit(&raw, &src);
        assert!(t.tail_break().is_none());
        assert!(t.last_line_shares_parent_line());
        assert_eq!(t.record.tail_budget_splits, old.record.tail_budget_splits, "13.7 as before");
        counted += t.record.tail_boundary_unavailable;
        assert!(t.record.tail_boundary_unavailable <= 1);
        if t.record.document_line_splits == 0 {
            assert_eq!(t.text, old.text);
        }
        seen += 1;
        if seen >= 200 {
            break;
        }
    }
    assert!(seen >= 200 && counted >= 5, "{seen} {counted}");
}

#[test]
fn a_later_edit_on_the_tail_line_reads_the_line_after_the_break() {
    // Item 39.1 with a boundary break: the earlier edit's tail is the parent
    // text up to the later edit; after the break, that text starts a new
    // line with the earlier edit's lead, and the later edit's column and
    // line are read there (it shares no line with the earlier edit's words).
    let mut r = Lcg(4206);
    let mut seen = 0;
    for _ in 0..20000 {
        let Some(p) = parent(&mut r, " ") else { continue };
        let pairs: Vec<(usize, usize)> = (0..p.sents.len()).flat_map(|a| (a + 2..p.sents.len()).map(move |b| (a, b))).filter(|&(a, b)| !p.text[p.sents[a].1..p.sents[b].0].contains('\n')).collect();
        if pairs.is_empty() {
            continue;
        }
        let (a, b) = pairs[r.below(pairs.len() as u64) as usize];
        let (r0, r1) = (raw_ending_long(&mut r, seen), synth::sentence(&mut r, true));
        let edits = [EditV2 { span: p.sents[a], insert_sep: None, raw: &r0 }, EditV2 { span: p.sents[b], insert_sep: None, raw: &r1 }];
        let slots: Vec<Slot> = edits.iter().map(|e| Slot { span: e.span, insert_sep: None, raw: Some(e.raw) }).collect();
        let laid = lay_out_item(&p.text, &none(), &slots);
        let (l0, l1) = (laid[0].as_ref().unwrap(), laid[1].as_ref().unwrap());
        let Some(tb) = l0.transferred.tail_break() else { continue };
        let between = &p.text[p.sents[a].1..p.sents[b].0];
        assert_eq!(l0.src.tail_column, between.chars().count());
        // the later edit's line: the lead and the parent text after the run
        let lead = &l0.transferred.text[l0.transferred.text.len() - tb.lead..];
        let line = l1.src.line.as_ref().unwrap();
        assert_eq!(line.before, format!("{lead}{}", &between[tb.run..]));
        assert!(line.widths.is_empty(), "no earlier edit's word on the line");
        assert!(l1.src.sibling_column);
        assert_eq!(l1.src.column, line.before.chars().count());
        // the item assembles, its parent bytes outside the extended range
        // unchanged, both final spans ending at a word
        let it = item_v2(&p.text, &none(), &edits);
        let asm = it.assembled.as_ref().unwrap_or_else(|| panic!("{:?}", it.reasons));
        assert_eq!(it.reps[0], l0.transferred.text);
        let (s0, e0) = p.sents[a];
        assert_eq!(&asm.text[..s0], &p.text[..s0]);
        let f0 = replacement_range(asm.edit_spans[0], &it.reps[0]);
        assert_eq!(f0, (s0, s0 + it.reps[0].len()));
        assert!(asm.text[f0.1..].starts_with(&p.text[e0 + tb.run..p.sents[b].0]));
        for &(x, y) in &asm.edit_spans {
            assert!(!asm.text[x..y].ends_with(char::is_whitespace) && !asm.text[x..y].starts_with(char::is_whitespace));
        }
        // the earlier edit, judged first with its sibling pending, is the same
        let mut pending = slots.clone();
        pending[1].raw = None;
        assert_eq!(grammar_a_form::profile::edit_in_item_v2(&p.text, &none(), &pending, 0).reps[0], it.reps[0]);
        seen += 1;
        if seen >= 10 {
            break;
        }
    }
    assert!(seen >= 10, "{seen}");
}

#[test]
fn child_v2_screens_the_document_with_the_boundary_break() {
    // child_v2 assembles the laid-out span with the boundary break (no
    // `form_assembly_error`), reads its constructed paragraphs over the
    // replacement's whole range and records the counter; the record is the
    // transfer's.
    let mut r = Lcg(4207);
    let mut seen = 0;
    for _ in 0..3000 {
        let Some(p) = parent(&mut r, " ") else { continue };
        let (s, e) = p.sents[r.below(p.sents.len() as u64) as usize];
        if tail_cols(&p.text, e) < 12 {
            continue;
        }
        let raw = raw_ending_long(&mut r, seen);
        let ch = child_v2(&p.text, Some((s, e)), &none(), &raw);
        let t = transfer(&raw, &none(), &SourceLayout::span(&p.text, s, e).unwrap());
        assert_eq!(ch.text, t.text);
        assert!(!ch.reasons.iter().any(|x| x.contains("assembly_error")), "{:?}", ch.reasons);
        if t.tail_break().is_none() {
            continue;
        }
        assert_eq!(ch.record["transfer"]["tail_boundary_breaks"], 1);
        assert_eq!(ch.record["layout_transfer"], "layout_transfer_v6");
        assert_eq!(grammar_a_form::profile::tail_boundary_breaks(&ch.record), 1);
        seen += 1;
        if seen >= 20 {
            break;
        }
    }
    assert!(seen >= 20, "{seen}");
}

#[test]
fn the_one_word_case_reads_the_continuation_lead_the_split_would_take() {
    // Item 42.3 with a non-empty continuation lead: the last word fits in W
    // less the tail's columns, but not in W less the lead the split would
    // take and the tail (item 13.7's budget). No suffix fits, so where the
    // boundary break exists the last line is not split: its words stay, the
    // boundary break is taken, and item 13.7 counts no split. (Reading the
    // lead as "" would find the word fitting, move it down with the lead,
    // and leave the one-word line beside the tail.)
    let mut r = Lcg(4208);
    let mut seen = 0;
    for _ in 0..20000 {
        let Some(p) = parent(&mut r, " ") else { continue };
        let (s, e) = p.sents[r.below(p.sents.len() as u64) as usize];
        let t = tail_cols(&p.text, e);
        if t < 12 {
            continue;
        }
        let mut src = SourceLayout::span(&p.text, s, e).unwrap();
        src.pools.cont_leads = vec!["  ".to_string()];
        let w = src.width.unwrap();
        if t + 8 > w {
            continue;
        }
        // the last word: within W less the tail, over W less the lead and the tail
        let l = w - t - r.below(2) as usize;
        assert!(l <= w - t && l > w - 2 - t);
        let raw = format!("{} {}.", synth::sentence(&mut r, false), "z".repeat(l - 1));
        let tr = transfer(&raw, &none(), &src);
        let Some(tb) = tr.tail_break() else { continue };
        let doc = splice(&p.text, (s, e), &tr.text).unwrap();
        let (_, tail) = replacement_tail(&tr.text).unwrap();
        let body_end = s + tr.text.len() - tail;
        let ls = doc[..body_end].rfind('\n').map_or(0, |k| k + 1);
        let last = &doc[ls..body_end];
        // the case: the last line holds more than the long word and, with the
        // tail, is over W, so item 13.7 reads it
        if last.split_whitespace().count() < 2 || cols(last) + t <= w {
            continue;
        }
        assert_eq!(tr.record.tail_budget_splits, 0, "{last:?}");
        assert_eq!(tr.record.tail_boundary_breaks, 1);
        assert_eq!(tb.lead, 2, "the boundary break takes the continuation lead");
        assert!(last.ends_with(&format!("{}.", "z".repeat(l - 1))), "{last:?}");
        assert!(cols(last) <= w, "{last:?}");
        assert!(!tr.last_line_shares_parent_line());
        // the parent's tail line starts with the lead
        assert_eq!(&doc[body_end + tb.run..body_end + tb.run + 3], "\n  ");
        seen += 1;
        if seen >= 20 {
            break;
        }
    }
    assert!(seen >= 20, "{seen}");
}
