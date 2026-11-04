//! Protocol A2 amendment 38 item 38.1 for `cc_cohort_profile_v2`, on
//! synthetic text only (no corpus text, no live call): item 13.7's split
//! extended to the assembled document line. The parents copy the shapes of
//! count 4's 159 lines (log index 125: local v1 and v2, spliced, rows and a
//! frontier `mixed` child, every one over W on the document reading only,
//! every one a document line of several words): hard-wrapped book prose
//! (project_gutenberg-like), IRC messages wrapped with a continuation indent
//! (ubuntu_irc-like) and reviews whose paragraphs open with an indent
//! (amazon review-like), with single spans, insertions, one-edit items and
//! two edits sharing a line. The geometry of each break is pinned in
//! `transfer`'s unit tests.
#[path = "support/a10_gen.rs"]
mod synth;

use grammar_a_form::conventions::{Choice, Conventions};
use grammar_a_form::measure::{LayoutClass, Measure};
use grammar_a_form::profile::{EditV2, Slot, child_v2, document_line_splits, edit_in_item_v2, item_v2, lay_out_item, replacement_lead, splice};
use grammar_a_form::transfer::{LAYOUT_TRANSFER_ID, SourceLayout, Transferred, transfer};
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

fn word_ranges(s: &str) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut start = None;
    for (i, c) in s.char_indices() {
        if c.is_whitespace() {
            if let Some(a) = start.take() {
                out.push((a, i));
            }
        } else if start.is_none() {
            start = Some(i);
        }
    }
    if let Some(a) = start {
        out.push((a, s.len()));
    }
    out
}

const LONG: [&str; 5] = ["Notwithstanding", "Characteristically", "Internationalisation", "Incomprehensibilities", "Counterrevolutionaries"];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Style {
    /// Book prose hard-wrapped at 66 to 72 columns, blank-line paragraphs.
    Book,
    /// IRC messages (`<nick> ...`), each wrapped at 72 with a four-space
    /// continuation indent, one per line group.
    Irc,
    /// A review: paragraphs whose first line is indented by four spaces,
    /// wrapped at 60, continuation lines flush.
    Review,
}

struct Parent {
    text: String,
    w: usize,
    /// Each paragraph's sentences' byte spans.
    sents: Vec<Vec<(usize, usize)>>,
}

/// Greedy wrap with a first-line and a continuation lead, each counted in
/// its line's budget.
fn wrap_leads(p: &str, w: usize, first: &str, cont: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut cur = String::new();
    for word in p.split_whitespace() {
        let lead = if out.is_empty() { first } else { cont };
        if cur.is_empty() {
            cur = format!("{lead}{word}");
        } else if cur.chars().count() + 1 + word.chars().count() <= w {
            cur.push(' ');
            cur.push_str(word);
        } else {
            out.push(std::mem::take(&mut cur));
            cur = format!("{cont}{word}");
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// A parent of `style`, or None when the measurement does not read it as a
/// prose hard-wrapped text whose every line of several words is within W.
fn parent(r: &mut Lcg, style: Style) -> Option<Parent> {
    let (w, paras, first, cont, sep, nick) = match style {
        Style::Book => ([66usize, 70, 72][r.below(3) as usize], 3, "", "", "\n\n", false),
        Style::Irc => (72, 4, "", "    ", "\n", true),
        Style::Review => (60, 3, "    ", "", "\n\n", false),
    };
    let mut blocks = Vec::new();
    let mut ranges: Vec<Vec<(usize, usize)>> = Vec::new();
    let mut wi = 0usize;
    for _ in 0..paras {
        let n = 3 + r.below(3) as usize;
        let sents: Vec<String> = (0..n).map(|_| synth::sentence(r, true)).collect();
        let mut t = String::new();
        if nick {
            t = format!("<user{}> ", r.below(9));
            wi += 1;
        }
        let mut rs = Vec::new();
        for s in &sents {
            let k = s.split_whitespace().count();
            rs.push((wi, wi + k));
            wi += k;
        }
        t.push_str(&sents.join(" "));
        blocks.push(wrap_leads(&t, w, first, cont).join("\n"));
        ranges.push(rs);
    }
    let text = blocks.join(sep);
    let wr = word_ranges(&text);
    let sents = ranges.iter().map(|ps| ps.iter().map(|&(a, b)| (wr[a].0, wr[b - 1].1)).collect()).collect();
    let c = Measure::of(&text).counts();
    let w = c.width?;
    let fits = text.split('\n').all(|l| cols(l) <= w || l.split_whitespace().count() < 2);
    (c.class != LayoutClass::LineForm && fits).then_some(Parent { text, w, sents })
}

fn raw(r: &mut Lcg) -> String {
    let mut s = (0..1 + r.below(2)).map(|_| synth::sentence(r, true)).collect::<Vec<_>>().join(" ");
    if r.below(3) == 0 {
        s = format!("{} {s}", LONG[r.below(5) as usize]);
    }
    if r.below(4) == 0 {
        s = format!("{} {}.", s.trim_end_matches('.'), LONG[r.below(5) as usize].to_lowercase());
    }
    s
}

/// An edit's place in its final document: its replacement's bytes (a
/// leading line break and lead included), its final span's start and its
/// wrapped units' byte ranges.
struct Placed {
    rep: (usize, usize),
    first: usize,
    wrapped: Vec<(usize, usize)>,
}

fn placed(t: &Transferred, at: usize) -> Placed {
    let first = at + replacement_lead(&t.text).unwrap();
    let wrapped = t.paragraphs.iter().zip(&t.unit_wrapped).filter(|x| *x.1).map(|(&(a, b), _)| (at + a, at + b)).collect();
    // the replacement's own bytes end at its last word: a trailing boundary
    // break (amendment 42 item 42.2) belongs to no word of it
    let tail = grammar_a_form::profile::replacement_tail(&t.text).unwrap().1;
    Placed { rep: (at, at + t.text.len() - tail), first, wrapped }
}

/// Count 4's document reading (amendment 29 item 29.2) on `doc`: each line
/// holding a word of an edit's wrapped unit, of more than one word, wider
/// than W. With each, whether item 38.1 gives it a break point (a word of an
/// edit's wrapped unit, not the line's first, after another of that edit's
/// words or starting the edit's final span), checked independently of the
/// crate's code.
fn document_reading(doc: &str, w: usize, edits: &[Placed]) -> Vec<(usize, bool)> {
    let mut out = Vec::new();
    let mut ls = 0;
    for line in doc.split('\n') {
        let words: Vec<(usize, usize)> = word_ranges(line).into_iter().map(|(a, b)| (ls + a, ls + b)).collect();
        let owner = |&(a, b): &(usize, usize)| edits.iter().position(|e| e.wrapped.iter().any(|&(x, y)| x <= a && b <= y));
        if words.iter().any(|x| owner(x).is_some()) && words.len() > 1 && cols(line) > w {
            let brk = (1..words.len()).any(|j| {
                owner(&words[j]).is_some_and(|k| {
                    let e = &edits[k];
                    (e.rep.0 <= words[j - 1].0 && words[j - 1].1 <= e.rep.1) || words[j].0 == e.first
                })
            });
            out.push((ls, brk));
        }
        ls += line.len() + 1;
    }
    out
}

/// Count 4's replacement reading (item 28.7) on a laid-out edit: each wrapped
/// unit's lines of more than one word, the first from the source's column
/// unless the replacement starts it on a new line, the last with its tail
/// while it still shares the parent's line (amendment 42 item 42.5: read
/// from the transfer).
fn replacement_reading(src: &SourceLayout, t: &Transferred) -> usize {
    let Some(w) = src.width else { return 0 };
    let n = t.paragraphs.len();
    let mut over = 0;
    for (u, &(s, e)) in t.paragraphs.iter().enumerate() {
        if !t.unit_wrapped[u] {
            continue;
        }
        let ls = t.text[..s].rfind('\n').map_or(0, |i| i + 1);
        let pieces: Vec<&str> = t.text[ls..e].split('\n').collect();
        for (i, piece) in pieces.iter().enumerate() {
            let mut c = cols(piece);
            if src.mixed && u == 0 && i == 0 && ls == 0 {
                c += src.column;
            }
            if src.mixed && u + 1 == n && i + 1 == pieces.len() && t.last_line_shares_parent_line() {
                c += src.tail_column;
            }
            over += usize::from(piece.split_whitespace().count() > 1 && c > w);
        }
    }
    over
}

/// The transfer as before amendment 38 (`SourceLayout::line` None: no split).
fn unsplit(raw: &str, src: &SourceLayout) -> Transferred {
    let mut s = src.clone();
    s.line = None;
    transfer(raw, &none(), &s)
}

#[derive(Default, Debug)]
struct Tally {
    children: usize,
    /// Lines over W on the document reading, before and after the rule.
    before: usize,
    after: usize,
    /// Of those after, lines with a break point (must be none).
    after_with_break: usize,
    splits: usize,
    replacement_before: usize,
    replacement_after: usize,
    /// Splits whose source has no continuation lead (each counted as a pool
    /// fallback), and splits with a non-empty lead.
    empty_pool_splits: usize,
    lead_splits: usize,
    /// Amendment 42: boundary breaks taken, item 13.7 one-word splits no
    /// longer made, and boundaries after a 2-space run.
    boundary_breaks: usize,
    kept_13_7: usize,
    two_space_runs: usize,
}

/// One single edit (a span, or an insertion with its separator) laid out
/// alone: before and after the rule, through every entry point.
fn single(p: &Parent, span: (usize, usize), sep: Option<&str>, raw: &str, t: &mut Tally) {
    let (s, e) = span;
    let src = match sep {
        Some(x) => SourceLayout::insertion(&p.text, s, x).unwrap(),
        None => SourceLayout::span(&p.text, s, e).unwrap(),
    };
    let new = transfer(raw, &none(), &src);
    let old = unsplit(raw, &src);
    // every entry point lays it out alike
    let edit = EditV2 { span, insert_sep: sep, raw };
    let it = item_v2(&p.text, &none(), &[edit]);
    assert_eq!(it.reps[0], new.text);
    assert_eq!(it.record["edits"][0]["transfer"], serde_json::to_value(&new.record).unwrap());
    let slots = [Slot { span, insert_sep: sep, raw: Some(raw) }];
    assert_eq!(edit_in_item_v2(&p.text, &none(), &slots, 0).reps[0], new.text);
    assert_eq!(lay_out_item(&p.text, &none(), &slots)[0].as_ref().unwrap().transferred.text, new.text);
    if sep.is_none() {
        let ch = child_v2(&p.text, Some(span), &none(), raw);
        assert_eq!(ch.text, new.text);
        assert_eq!(ch.record["transfer"], it.record["edits"][0]["transfer"]);
        assert!(!ch.reasons.iter().any(|x| x.starts_with("form_assembly_error")), "{:?}", ch.reasons);
        assert_eq!(document_line_splits(&ch.record), new.record.document_line_splits as u64);
    }
    assert_eq!(document_line_splits(&it.record), new.record.document_line_splits as u64);
    // the words, their order and every other count are unchanged; each
    // application adds one line break. Amendment 42: a boundary break (b)
    // adds one too, and where it exists item 13.7's one-word case no longer
    // splits (s137: the split `old`, laid out without the document line, made
    // and `new` did not).
    let k = new.record.document_line_splits;
    let b = new.record.tail_boundary_breaks;
    let s137 = old.record.tail_budget_splits - new.record.tail_budget_splits;
    assert!(b <= 1 && s137 <= b, "13.7 keeps its line only where the boundary break is then taken: {b} {s137}");
    t.boundary_breaks += b;
    t.kept_13_7 += s137;
    assert_eq!(new.record.tail_boundary_unavailable, 0, "these parents leave a whitespace run after every edit");
    assert_eq!(new.text.split_whitespace().collect::<Vec<_>>(), old.text.split_whitespace().collect::<Vec<_>>());
    assert_eq!(new.text.matches('\n').count() + s137, old.text.matches('\n').count() + k + b);
    let lead_break = usize::from(new.text.starts_with('\n') && !old.text.starts_with('\n'));
    assert_eq!(new.record.child_lines + s137, old.record.child_lines + k - lead_break);
    // a boundary break ends the replacement with the parent's run after the
    // edit, one line break and the next continuation lead
    if b == 1 {
        let tb = new.tail_break().unwrap();
        let tail = &new.text[new.text.len() - tb.run - 1 - tb.lead..];
        let after = &p.text[e..];
        let run = &after[..after.len() - after.trim_start_matches([' ', '\t']).len()];
        assert_eq!(&tail[..tb.run], run, "the parent's run, byte for byte");
        assert_eq!(&tail[tb.run..tb.run + 1], "\n");
        assert!(src.pools.cont_leads.is_empty() || src.pools.cont_leads.contains(&tail[tb.run + 1..].to_string()));
        assert!(!new.last_line_shares_parent_line());
        t.two_space_runs += usize::from(tb.run == 2);
    } else {
        assert!(new.tail_break().is_none() && new.last_line_shares_parent_line());
    }
    // the unit and body ranges follow the text: the same words, unit by unit,
    // each range starting and ending on a word
    assert_eq!(new.paragraphs.len(), old.paragraphs.len());
    let words = |t: &str, (a, b): (usize, usize)| t[a..b].split_whitespace().map(str::to_string).collect::<Vec<_>>();
    for (&n, &o) in new.paragraphs.iter().zip(&old.paragraphs) {
        assert_eq!(words(&new.text, n), words(&old.text, o));
        assert!(!new.text[n.0..n.1].starts_with(char::is_whitespace) && !new.text[n.0..n.1].ends_with(char::is_whitespace));
    }
    assert_eq!(new.body.map(|b| words(&new.text, b)), old.body.map(|b| words(&old.text, b)));
    // each new line takes the source's next continuation lead, and the line
    // left before a break between two words the next wrap trail (amendment
    // 39 item 39.3); an empty pool gives "" and is counted
    // (the split 13.7 no longer makes took a lead and left a wrap-trailed
    // line before it; a boundary break takes a lead, counted only as a pick,
    // and no trail: the new line holds parent text only)
    let pool = &src.pools.cont_leads;
    let inner = k - lead_break;
    let trails = &src.pools.wrap_trails;
    let n = |x: usize| x as isize;
    assert_eq!(
        n(new.record.ws_pool_fallbacks) - n(old.record.ws_pool_fallbacks),
        if pool.is_empty() { n(k + b) - n(s137) } else { 0 } + if trails.is_empty() { n(inner) - n(s137) } else { 0 }
    );
    let trailing = n(new.record.trailing_ws_lines) - n(old.record.trailing_ws_lines);
    if trails.iter().all(String::is_empty) {
        assert_eq!(trailing, 0);
    } else if trails.iter().all(|l| !l.is_empty()) {
        assert_eq!(trailing, n(inner) - n(s137));
    }
    let leads = n(new.record.leading_ws_lines) - n(old.record.leading_ws_lines);
    if pool.iter().all(String::is_empty) {
        assert_eq!(leads, 0);
    } else if pool.iter().all(|l| !l.is_empty()) {
        assert_eq!(leads, n(k) - n(s137));
        t.lead_splits += k + b;
    }
    t.empty_pool_splits += if pool.is_empty() { k + b } else { 0 };
    let mut rec = serde_json::to_value(&new.record).unwrap();
    let rec_old = serde_json::to_value(&old.record).unwrap();
    let counters = ["document_line_splits", "tail_boundary_breaks"];
    if k == 0 && b == 0 {
        assert_eq!(new.text, old.text);
        assert_eq!(rec, rec_old, "a record the rule does not change keeps its bytes");
        for m in counters {
            assert!(rec.get(m).is_none(), "{m}");
        }
    } else {
        assert_eq!((rec.get("document_line_splits").and_then(|x| x.as_u64()).unwrap_or(0), rec.get("tail_boundary_breaks").and_then(|x| x.as_u64()).unwrap_or(0)), (k as u64, b as u64));
        for m in ["child_lines", "leading_ws_lines", "trailing_ws_lines", "ws_pool_fallbacks", "tail_budget_splits"] {
            rec[m] = rec_old[m].clone();
        }
        let mut ro = rec_old.clone();
        for m in counters {
            ro.as_object_mut().unwrap().remove(m);
            rec.as_object_mut().unwrap().remove(m);
        }
        assert_eq!(rec, ro);
    }
    // the documents
    let pre = sep.map_or(0, str::len);
    let doc_of = |rep: &str| match sep {
        Some(x) => format!("{}{x}{rep}{}", &p.text[..s], &p.text[s..]),
        None => splice(&p.text, span, rep).unwrap(),
    };
    let (d_new, d_old) = (doc_of(&new.text), doc_of(&old.text));
    let r_new = document_reading(&d_new, p.w, &[placed(&new, s + pre)]);
    let r_old = document_reading(&d_old, p.w, &[placed(&old, s + pre)]);
    t.children += 1;
    t.before += r_old.len();
    t.after += r_new.len();
    // amendment 42 item 42.5: the replacement reading's last line takes the
    // parent's tail only while it still shares the parent's line
    if b == 1 {
        let last = new.text[..new.paragraphs.last().unwrap().1].rsplit('\n').next().unwrap();
        assert!(cols(last) <= p.w || last.split_whitespace().count() == 1, "{last:?}");
    }
    t.after_with_break += r_new.iter().filter(|x| x.1).count();
    t.splits += k;
    t.replacement_before += replacement_reading(&src, &old);
    t.replacement_after += replacement_reading(&src, &new);
}

/// A sweep of single edits in parents of `style`.
fn sweep(style: Style, seed: u64, n: usize) -> Tally {
    let mut r = Lcg(seed);
    let mut t = Tally::default();
    let mut tries = 0;
    while t.children < n && tries < 20 * n {
        tries += 1;
        let Some(p) = parent(&mut r, style) else { continue };
        let ss = &p.sents[r.below(p.sents.len() as u64) as usize];
        let k = r.below(ss.len() as u64) as usize;
        let rw = raw(&mut r);
        if r.below(3) == 0 {
            single(&p, (ss[k].1, ss[k].1), Some(" "), &rw, &mut t);
        } else {
            let k2 = (k + r.below(2) as usize).min(ss.len() - 1);
            single(&p, (ss[k].0, ss[k2].1), None, &rw, &mut t);
        }
    }
    assert_eq!(t.children, n, "{style:?}: {tries} tries");
    t
}

#[test]
fn single_edits_in_the_three_parent_shapes_leave_no_splittable_line_over_w() {
    // Before the rule, each shape gives document lines over W on the
    // document reading only (the replacement reading is 0): a first word
    // with no room after the parent's column, or one word followed by
    // parent text that does not fit. After it, no line over W has a break
    // point, and the replacement reading stays 0.
    let (mut empty_pool, mut leads) = (0, 0);
    for (style, seed) in [(Style::Book, 3801u64), (Style::Irc, 3802), (Style::Review, 3803)] {
        let t = sweep(style, seed, 300);
        empty_pool += t.empty_pool_splits;
        leads += t.lead_splits;
        eprintln!("{style:?}: {t:?}");
        assert_eq!((t.replacement_before, t.replacement_after), (0, 0), "{style:?}: {t:?}");
        assert!(t.before >= 10, "{style:?}: the shape reproduces count 4's lines: {t:?}");
        assert_eq!(t.after_with_break, 0, "{style:?}: {t:?}");
        // amendment 42: no line over W is left (layout_transfer_v5 left the
        // one-word lines with the parent's tail); both of its cases occur
        assert_eq!(t.after, 0, "{style:?}: {t:?}");
        assert!(t.boundary_breaks >= 5 && t.kept_13_7 >= 1 && t.boundary_breaks > t.kept_13_7, "{style:?}: {t:?}");
        assert!(t.splits + t.boundary_breaks >= t.before, "{style:?}: {t:?}");
    }
    assert!(empty_pool >= 1 && leads >= 1, "splits from an empty pool ({empty_pool}) and with an indent lead ({leads})");
}

#[test]
fn a_first_word_with_no_room_after_the_parents_column_starts_a_new_line() {
    // Item 29.2's example: a single edit's first word longer than its budget
    // after the parent's column. It moves to a new line with the edit's
    // continuation lead, and the replacement begins with that line break.
    for (style, seed) in [(Style::Book, 3811u64), (Style::Irc, 3812), (Style::Review, 3813)] {
        let mut r = Lcg(seed);
        let mut seen = 0;
        let mut indented = 0;
        for _ in 0..400 {
            let Some(p) = parent(&mut r, style) else { continue };
            // a sentence that starts late on its line
            let Some(&(s, e)) = p.sents.iter().flatten().find(|&&(s, _)| {
                let ls = p.text[..s].rfind('\n').map_or(0, |k| k + 1);
                !p.text[ls..s].trim().is_empty() && p.text[ls..s].chars().count() + LONG[4].len() > p.w
            }) else {
                continue;
            };
            let rw = format!("{} {}", LONG[4], synth::sentence(&mut r, true));
            let src = SourceLayout::span(&p.text, s, e).unwrap();
            let t = transfer(&rw, &none(), &src);
            let old = unsplit(&rw, &src);
            assert!(old.text.starts_with(LONG[4]), "alone on the shared line before the rule");
            // the next continuation lead after the layout's own picks
            let pool = &src.pools.cont_leads;
            let lead = if pool.is_empty() { String::new() } else { pool[old.cont_picks % pool.len()].clone() };
            assert!(t.text.starts_with(&format!("\n{lead}{}", LONG[4])), "{style:?}: {:?}", t.text);
            assert_eq!(replacement_lead(&t.text), Some(1 + lead.len()));
            assert!(t.record.document_line_splits >= 1);
            if style == Style::Irc && !pool.is_empty() {
                assert_eq!(lead, "    ", "the IRC parent's continuation indent");
                indented += 1;
            }
            // the parent's line keeps its bytes, the space before the span included
            let doc = splice(&p.text, (s, e), &t.text).unwrap();
            assert_eq!(&doc[..s], &p.text[..s]);
            assert!(document_reading(&doc, p.w, &[placed(&t, s)]).iter().all(|x| !x.1));
            seen += 1;
            if seen >= 20 {
                break;
            }
        }
        assert!(seen >= 20, "{style:?}: {seen}");
        assert!(style != Style::Irc || indented >= 5, "{indented}");
    }
}

#[test]
fn a_line_whose_overflow_lies_in_parent_text_takes_the_boundary_break() {
    // An edit whose one word starts its document line, followed by the rest
    // of the parent's line: the line may be over W, and no break point lies
    // inside the edit's text. layout_transfer_v5 left it (count 4 read it);
    // amendment 42 item 42.1 breaks after the edit's word, so the word's
    // line holds one word (exempt) and the parent text, unchanged and not
    // re-wrapped, starts the next line.
    let mut r = Lcg(3821);
    let mut seen = 0;
    for _ in 0..400 {
        let Some(p) = parent(&mut r, Style::Book) else { continue };
        // the first word of a line that is not a paragraph's first, with at
        // least 12 columns of parent text after it
        let lines: Vec<(usize, &str)> = p.text.split('\n').scan(0usize, |at, l| {
            let x = (*at, l);
            *at += l.len() + 1;
            Some(x)
        }).collect();
        let Some(&(ls, l)) = lines.iter().skip(1).find(|(_, l)| !l.trim().is_empty() && cols(l) - l.split(' ').next().unwrap().len() + LONG[4].len() > p.w) else { continue };
        let first = l.split(' ').next().unwrap();
        let span = (ls, ls + first.len());
        let t = transfer(LONG[4], &none(), &SourceLayout::span(&p.text, span.0, span.1).unwrap());
        if t.unit_wrapped.first() != Some(&true) {
            continue;
        }
        let lead = &t.text[LONG[4].len() + 1 + 1..];
        assert_eq!(t.text, format!("{} \n{lead}", LONG[4]), "the parent's one space, a line break and the lead");
        assert_eq!((t.record.document_line_splits, t.record.tail_boundary_breaks), (0, 1));
        let doc = splice(&p.text, span, &t.text).unwrap();
        assert_eq!(&doc[..span.0], &p.text[..span.0]);
        assert_eq!(&doc[span.0 + t.text.len()..], &p.text[span.1 + 1..], "the parent resumes at its first non-whitespace byte");
        let reading = document_reading(&doc, p.w, &[placed(&t, span.0)]);
        assert!(reading.is_empty(), "no line over W is read: {reading:?}");
        seen += 1;
        if seen >= 10 {
            break;
        }
    }
    assert!(seen >= 10, "{seen}");
}

#[test]
fn a_one_word_line_is_exempt() {
    // An edit of one word wider than W, alone on its document line.
    let mut r = Lcg(3831);
    let p = (0..50).find_map(|_| parent(&mut r, Style::Book)).unwrap();
    let long = "x".repeat(p.w + 5);
    // the whole first line of the second paragraph
    let start = p.text.find("\n\n").unwrap() + 2;
    let end = start + p.text[start..].find('\n').unwrap();
    let src = SourceLayout::span(&p.text, start, end).unwrap();
    let t = transfer(&long, &none(), &src);
    assert_eq!((t.text.as_str(), t.record.document_line_splits), (long.as_str(), 0));
}

/// Two edits whose first ends and second starts on one parent line (as
/// `a29_rules`' cases), laid out as an item.
fn shared_items(style: Style, seed: u64, n: usize) -> Tally {
    let mut r = Lcg(seed);
    let mut t = Tally::default();
    let mut tries = 0;
    while t.children < n && tries < 8000 {
        tries += 1;
        let Some(p) = parent(&mut r, style) else { continue };
        let ss = &p.sents[r.below(p.sents.len() as u64) as usize];
        let pairs: Vec<(usize, usize)> = (0..ss.len()).flat_map(|a| (a + 2..ss.len()).map(move |b| (a, b))).filter(|&(a, b)| !p.text[ss[a].1..ss[b].0].contains('\n')).collect();
        if pairs.is_empty() {
            continue;
        }
        let (a, b) = pairs[r.below(pairs.len() as u64) as usize];
        let (r1, r2) = (raw(&mut r), raw(&mut r));
        let edits = [EditV2 { span: ss[a], insert_sep: None, raw: &r1 }, EditV2 { span: ss[b], insert_sep: None, raw: &r2 }];
        let it = item_v2(&p.text, &none(), &edits);
        let Some(asm) = &it.assembled else { panic!("{:?}", it.reasons) };
        let slots: Vec<Slot> = edits.iter().map(|e| Slot { span: e.span, insert_sep: None, raw: Some(e.raw) }).collect();
        let laid = lay_out_item(&p.text, &none(), &slots);
        let mut pl = Vec::new();
        for (k, l) in laid.iter().enumerate() {
            let l = l.as_ref().unwrap();
            assert_eq!(l.transferred.text, it.reps[k]);
            let at = asm.edit_spans[k].0 - replacement_lead(&it.reps[k]).unwrap();
            pl.push(placed(&l.transferred, at));
            t.splits += l.transferred.record.document_line_splits;
            assert_eq!(edit_in_item_v2(&p.text, &none(), &slots, k).reps[0], it.reps[k]);
        }
        // Item 29.1's order: the earlier edit judged first, its later sibling
        // pending, keeps the text the item check gives it (its document line
        // is read up to the next edit only), so a later sibling never changes
        // it; the later edit reads the earlier one's final text before it.
        let (e0, e1) = if edits[0].span < edits[1].span { (0, 1) } else { (1, 0) };
        // the earlier edit's line is read up to the later edit only: laid
        // out alone with that tail and that line, it is the item's
        let (sp0, sp1) = (edits[e0].span, edits[e1].span);
        let mut s0 = SourceLayout::span(&p.text, sp0.0, sp0.1).unwrap();
        let between = &p.text[sp0.1..sp1.0];
        s0.tail_column = between.chars().count();
        s0.line.as_mut().unwrap().after = between.to_string();
        assert_eq!(transfer(edits[e0].raw, &none(), &s0).text, it.reps[e0]);
        let mut pending = slots.clone();
        pending[e1].raw = None;
        assert_eq!(edit_in_item_v2(&p.text, &none(), &pending, e0).reps[0], it.reps[e0]);
        let l1 = laid[e1].as_ref().unwrap();
        if l1.src.sibling_column {
            let s1 = asm.edit_spans[e1].0 - replacement_lead(&it.reps[e1]).unwrap();
            let ls = asm.text[..s1].rfind('\n').map_or(0, |x| x + 1);
            let before = &asm.text[ls..s1];
            let line = l1.src.line.as_ref().unwrap();
            assert_eq!(line.before, before);
            // the earlier edit's wrapped units on that line, with its W
            // (amendment 39 item 39.1: the smallest W of the line's edits)
            let w0 = laid[e0].as_ref().unwrap().src.width.unwrap();
            let want: Vec<((usize, usize), usize)> = pl[e0].wrapped.iter().filter(|r| r.1 > ls).map(|&(a, b)| ((a.max(ls) - ls, b - ls), w0)).collect();
            // after the earlier edit's boundary break (amendment 42) the
            // later edit's line starts with the lead and the parent text:
            // it holds none of the earlier edit's words
            let b0 = laid[e0].as_ref().unwrap().transferred.tail_break();
            assert_eq!(want.is_empty(), b0.is_some(), "{want:?} {b0:?}");
            t.boundary_breaks += usize::from(b0.is_some());
            assert_eq!(line.widths, want);
            for ((a, b), _) in &line.widths {
                assert!(!before[*a..*b].trim().is_empty());
            }
        }
        for l in laid.iter().flatten() {
            t.boundary_breaks += l.transferred.record.tail_boundary_breaks;
        }
        let reading = document_reading(&asm.text, p.w, &pl);
        t.children += 1;
        t.after += reading.len();
        t.after_with_break += reading.iter().filter(|x| x.1).count();
    }
    assert_eq!(t.children, n, "{style:?}");
    t
}

#[test]
fn two_edits_sharing_a_line_leave_no_splittable_line_over_w() {
    let mut splits = 0;
    for (style, seed, n) in [(Style::Book, 3841u64, 150), (Style::Irc, 3842, 150), (Style::Review, 3843, 50)] {
        let t = shared_items(style, seed, n);
        eprintln!("{style:?}: {t:?}");
        assert_eq!(t.after_with_break, 0, "{style:?}: {t:?}");
        splits += t.splits;
    }
    assert!(splits >= 5, "the rule applies to items' edits: {splits}");
}

#[test]
fn whole_text_children_are_unchanged() {
    // A mirror's lines hold only its own words within budget: the rule never
    // applies, and the record has no counter.
    for style in [Style::Book, Style::Irc, Style::Review] {
        let mut r = Lcg(3851);
        let mut n = 0;
        for _ in 0..100 {
            let Some(p) = parent(&mut r, style) else { continue };
            let rw = (0..8).map(|_| synth::sentence(&mut r, true)).collect::<Vec<_>>().join(" ");
            let c = child_v2(&p.text, None, &none(), &rw);
            assert!(c.record["transfer"].get("document_line_splits").is_none());
            assert_eq!(c.text, unsplit(&rw, &SourceLayout::whole(&p.text)).text);
            n += 1;
        }
        assert!(n >= 20, "{style:?}: {n}");
    }
}

#[test]
fn the_layout_id_is_v6_and_splice_accepts_the_leading_break() {
    assert_eq!(LAYOUT_TRANSFER_ID, "layout_transfer_v6");
    assert_eq!(LAYOUT_TRANSFER_ID.len(), "layout_transfer_v5".len());
    let p = "Alpha one. Beta two.";
    assert_eq!(splice(p, (11, 20), "\n  Gamma three.").unwrap(), "Alpha one. \n  Gamma three.");
    assert_eq!(splice(p, (11, 20), "\n\nGamma."), Err("replacement layout"));
    assert_eq!(splice(p, (11, 20), "\n  "), Err("replacement layout"));
    assert_eq!(splice(p, (11, 20), "Gamma.\n"), Err("replacement layout"));
}

#[test]
fn an_earlier_edits_line_before_an_insertion_whose_separator_breaks_it_ends_at_the_separator() {
    // Amendment 39 item 39.1: the earlier edit's last document line in the
    // assembled item ends where the insertion's separator breaks it: the
    // parent text up to the point and the separator up to its line break.
    // With a separator that holds no line break, the line is read up to the
    // insertion only. Item 29.1's tail stays the parent's line there.
    let mut r = Lcg(3861);
    let mut seen = 0;
    for _ in 0..800 {
        let Some(p) = parent(&mut r, Style::Book) else { continue };
        let ss = &p.sents[r.below(p.sents.len() as u64) as usize];
        let Some(a) = (0..ss.len()).find(|&a| (a + 1..ss.len()).any(|b| !p.text[ss[a].1..ss[b].1].contains('\n'))) else { continue };
        let b = (a + 1..ss.len()).rev().find(|&b| !p.text[ss[a].1..ss[b].1].contains('\n')).unwrap();
        let (s0, e0, at) = (ss[a].0, ss[a].1, ss[b].1);
        let raw0 = raw(&mut r);
        let alone = SourceLayout::span(&p.text, s0, e0).unwrap();
        for (sep, after) in [("\n\n", ""), (" \n", " "), ("\n", ""), (" ", "")] {
            let slots = [Slot { span: (s0, e0), insert_sep: None, raw: Some(&raw0) }, Slot { span: (at, at), insert_sep: Some(sep), raw: Some("Inserted words stand here.") }];
            let laid = lay_out_item(&p.text, &none(), &slots);
            let l0 = laid[0].as_ref().unwrap();
            let between = &p.text[e0..at];
            assert_eq!(l0.src.line.as_ref().unwrap().after, format!("{between}{after}"), "{sep:?}");
            let tail = if sep.contains('\n') { alone.tail_column } else { between.chars().count() };
            assert_eq!(l0.src.tail_column, tail, "{sep:?}");
            // laid out alone with that context, it is the item's; a pending
            // insertion gives the same text (its place is known)
            let mut s = alone.clone();
            s.tail_column = tail;
            s.line.as_mut().unwrap().after = format!("{between}{after}");
            assert_eq!(transfer(&raw0, &none(), &s).text, l0.transferred.text);
            let mut pending = slots;
            pending[1].raw = None;
            assert_eq!(edit_in_item_v2(&p.text, &none(), &pending, 0).reps[0], l0.transferred.text);
        }
        seen += 1;
        if seen >= 20 {
            break;
        }
    }
    assert!(seen >= 20, "{seen}");
}

#[test]
fn a_break_between_two_words_takes_the_flowed_parents_next_wrap_trail() {
    // Amendment 39 item 39.3 on a parent whose wrapped lines keep one
    // trailing space (as format=flowed mail does). The text after the edit
    // on its last line is set to one word wider than W, glued to the edit's
    // last word (as after an insertion with no whitespace there), so no
    // moved part fits and there is no boundary break (amendment 42 item
    // 42.1, counted): the last break point, between the last line's third-
    // and second-last words, is taken. The line left before it takes the
    // next wrap trail, counted, and the new line (those two words and the
    // long one) is left.
    let mut r = Lcg(3871);
    let mut seen = 0;
    for _ in 0..800 {
        let Some(p) = parent(&mut r, Style::Book) else { continue };
        let lines: Vec<&str> = p.text.split('\n').collect();
        let text: String = (0..lines.len())
            .map(|i| if !lines[i].is_empty() && lines.get(i + 1).is_some_and(|n| !n.is_empty()) { format!("{} ", lines[i]) } else { lines[i].to_string() })
            .collect::<Vec<_>>()
            .join("\n");
        // a sentence of the flowed text, found by its words
        let flat: Vec<(usize, usize)> = p.sents.iter().flatten().copied().collect();
        let (s0, e0) = flat[r.below(flat.len() as u64) as usize];
        let wi = |x: usize| word_ranges(&p.text).iter().position(|w| w.0 == x || w.1 == x).unwrap();
        let wr = word_ranges(&text);
        let span = (wr[wi(s0)].0, wr[wi(e0)].1);
        let Some(mut src) = SourceLayout::span(&text, span.0, span.1) else { continue };
        let Some(w) = src.width else { continue };
        if src.pools.wrap_trails.is_empty() || src.pools.wrap_trails.iter().any(String::is_empty) {
            continue;
        }
        src.line.as_mut().unwrap().after = "y".repeat(w);
        let rw = format!("{} {}", raw(&mut r), raw(&mut r));
        let new = transfer(&rw, &none(), &src);
        let old = unsplit(&rw, &src);
        let last = old.text.rsplit('\n').next().unwrap();
        // (a first word with no room after the parent's column is moved
        // first, as in the other tests: such cases are skipped here)
        if new.unit_wrapped.last() != Some(&true) || last.split_whitespace().count() < 3 || new.text.starts_with('\n') != old.text.starts_with('\n') {
            continue;
        }
        assert_eq!(new.record.document_line_splits, 1, "{:?}", new.text);
        assert_eq!((new.record.tail_boundary_breaks, new.record.tail_boundary_unavailable), (0, 1));
        let trail = &src.pools.wrap_trails[old.trail_picks % src.pools.wrap_trails.len()];
        let k = old.text[..old.text.rfind(' ').unwrap()].rfind(' ').unwrap();
        let pool = &src.pools.cont_leads;
        let lead = if pool.is_empty() { "" } else { pool[old.cont_picks % pool.len()].as_str() };
        assert_eq!(new.text, format!("{}{trail}\n{lead}{}", &old.text[..k], &old.text[k + 1..]));
        assert_eq!(new.record.trailing_ws_lines, old.record.trailing_ws_lines + 1);
        assert_eq!(new.record.ws_pool_fallbacks, old.record.ws_pool_fallbacks + usize::from(pool.is_empty()));
        assert_eq!(new.record.child_lines, old.record.child_lines + 1);
        assert_eq!(new.trail_picks, old.trail_picks + 1);
        // with no wrap trail in the pool the break takes none, and the empty
        // pool is counted
        let mut bare = src.clone();
        bare.pools.wrap_trails.clear();
        let (nb, ob) = (transfer(&rw, &none(), &bare), unsplit(&rw, &bare));
        let kb = ob.text[..ob.text.rfind(' ').unwrap()].rfind(' ').unwrap();
        assert_eq!(nb.text, format!("{}\n{lead}{}", &ob.text[..kb], &ob.text[kb + 1..]));
        assert_eq!(nb.record.trailing_ws_lines, ob.record.trailing_ws_lines);
        assert_eq!(nb.record.ws_pool_fallbacks, ob.record.ws_pool_fallbacks + 1 + usize::from(pool.is_empty()));
        seen += 1;
        if seen >= 20 {
            break;
        }
    }
    assert!(seen >= 20, "{seen}");
}
