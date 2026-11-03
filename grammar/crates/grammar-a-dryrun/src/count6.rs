//! Count 6 on item 13.1's constructed final paragraph (protocol A2
//! amendment 31 items 31.1-31.3; ruling W8-c), on one child's landed
//! transfer.
//!
//! **The population** is item 28.9's, unchanged: every body unit with a
//! placed break on either edge (two body units of one `place_breaks`
//! segment; `invariants`). Body units with no placed break on either edge
//! are read too, for item 31.2's figure at fixed boundaries.
//!
//! **The paragraph** is the constructed final paragraph holding the unit,
//! as the landed screen builds it (`grammar_a_form::profile`'s
//! `constructed_paragraphs`, exposed read-only; this module copies no
//! rule, item 14.1):
//! * a whole text (a mirror or a donor row): the unit itself;
//! * a span child, an insertion or an edit of an item: two readings, each
//!   where its document assembles:
//!   - **the item** (`ItemDoc`): the item assembled from every edit that has
//!     a re-derived laid-out child, as the dry run's final document is
//!     built, each edit with its own units, so a sibling's generated text
//!     and its unit boundaries cut the join and the parent words it
//!     replaced are not counted;
//!   - **the edit alone in its parent** (`profile::splice` for a span, as
//!     `child_v2` assembles it, else `profile::assemble_item` over the one
//!     edit, as `item_v2` does);
//!
//!   the unit counts when either reading gives fewer than
//!   `MIN_PARAGRAPH_WORDS` (8) words (`text::words`), and when neither
//!   assembles, on its own words.
//!
//! The arithmetic reading (`pre_words`/`post_words`) is never the gate. A
//! unit whose arithmetic word count differs from a constructed reading's is
//! `differs` (`c6_construction_differs`). A unit that lies in no constructed
//! paragraph, or in several, is counted (`not_one_paragraph`) and takes the
//! fewest words of those it overlaps (none: that reading is absent).
//!
//! **The re-measured reading** (item 31.2): the `layout_measure_v2`
//! paragraphs of the assembled document (the item's, else the edit alone
//! in its parent, else the whole text) that overlap the unit, when one
//! holds fewer than 8 words (`remeasured_short`). Whether that is item
//! 13.10's figure 1 is read from the child's text-free record by the caller
//! (`tally`; item 31.3's fail-safe).
use grammar_a_form::layout::MIN_PARAGRAPH_WORDS;
use grammar_a_form::measure::Measure;
use grammar_a_form::profile::{self, Part, Placed};
use grammar_a_form::text;
use grammar_a_form::transfer::{SourceLayout, Transferred, UnitRole};

/// One edit of an item that has a re-derived laid-out child, placed in the
/// item's assembled document: the parent range it replaces (`s == e` for an
/// insertion), an insertion's separator, the replacement's byte range in the
/// document (its leading line break and lead included) and its units (byte
/// ranges in the replacement).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ItemEdit {
    pub span: (usize, usize),
    pub sep: Option<String>,
    pub fin: (usize, usize),
    pub units: Vec<(usize, usize)>,
}

/// The item reading's document (item 31.1): the parent, the item assembled
/// from every edit that has a re-derived laid-out child, those edits in
/// item order, and the counted child's index among them.
#[derive(Clone, Copy, Debug)]
pub struct ItemDoc<'a> {
    pub parent: &'a str,
    pub doc: &'a str,
    pub edits: &'a [ItemEdit],
    pub index: usize,
}

/// The counted child's own edit in its parent: its span (`s == e` for an
/// insertion) and an insertion's separator. None for a whole text.
#[derive(Clone, Copy, Debug)]
pub struct Own<'a> {
    pub parent: &'a str,
    pub span: (usize, usize),
    pub sep: Option<&'a str>,
}

/// One body unit's readings.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Unit6 {
    /// The unit's index in the child.
    pub unit: usize,
    /// A placed break on either edge (item 28.9's population).
    pub placed: bool,
    /// Its own words (item 28.9's former gate).
    pub own: usize,
    /// Its own words with `pre_words` and `post_words` (place_breaks'
    /// arithmetic; reported beside).
    pub arithmetic: usize,
    /// The item reading's words, where the item assembles.
    pub item: Option<usize>,
    /// The edit-alone reading's words, where it assembles.
    pub alone: Option<usize>,
    /// The constructed reading is under 8 words: either reading, or its own
    /// words when neither exists (a whole text's is its own words).
    pub short: bool,
    /// The arithmetic word count differs from a constructed reading's.
    pub differs: bool,
    /// A re-measured paragraph of the assembled document overlapping it
    /// holds fewer than 8 words (placed units only).
    pub remeasured_short: bool,
}

/// Count 6's readings on one child.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Count6 {
    /// Every body unit, in order.
    pub units: Vec<Unit6>,
    /// Readings in which the unit lay in no constructed paragraph, or in
    /// several.
    pub not_one_paragraph: usize,
    /// Why the item document cannot be read (the caller stops).
    pub inconsistent: Option<&'static str>,
}

/// Words of the constructed paragraphs of `doc` overlapping each body unit
/// (`units`, byte ranges in the replacement at `fin`): the fewest, or None
/// when none overlaps; `bad` counts units not in exactly one.
fn reading(parent: &str, doc: &str, placed: &[Placed], fin: (usize, usize), units: &[(usize, usize)], body: &[usize], bad: &mut usize) -> Vec<Option<usize>> {
    let paras = profile::constructed_paragraphs(parent, doc, placed);
    body.iter()
        .map(|&u| {
            let (s, e) = units[u];
            let ov = profile::overlapping(&paras, (fin.0 + s, fin.0 + e));
            if ov.len() != 1 {
                *bad += 1;
            }
            ov.iter().map(|&(a, b)| text::words(&doc[a..b])).min()
        })
        .collect()
}

/// The edit alone in its parent: the document and the replacement's range
/// in it, when it assembles (`profile::splice` for a span, else
/// `profile::assemble_item` over the one edit).
fn alone_doc(o: Own, rep: &str) -> Option<(String, (usize, usize))> {
    if o.span.0 < o.span.1
        && let Ok(d) = profile::splice(o.parent, o.span, rep)
    {
        return Some((d, (o.span.0, o.span.0 + rep.len())));
    }
    let a = profile::assemble_item(o.parent, &[Part { span: o.span, insert_sep: o.sep }], &[rep]).ok()?;
    let lead = profile::replacement_lead(rep).unwrap_or(0);
    Some((a.text, (a.edit_spans[0].0 - lead, a.edit_spans[0].1)))
}

/// Count 6's readings on `t`, laid out from `src`: `own` is the child's
/// edit in its parent (None for a whole text) and `item` the item reading's
/// document (None when the child's item has no other edit or does not
/// assemble).
pub fn count6(src: &SourceLayout, t: &Transferred, own: Option<Own>, item: Option<ItemDoc>) -> Count6 {
    let mut out = Count6::default();
    let units = &t.diag.units;
    let n = t.paragraphs.len();
    if units.len() != n {
        out.inconsistent = Some("diagnostics do not cover the units");
        return out;
    }
    let placed_edge = |a: usize, b: usize| units[a].role == UnitRole::Body && units[b].role == UnitRole::Body && units[a].segment.is_some() && units[a].segment == units[b].segment;
    let body: Vec<usize> = (0..n).filter(|&u| units[u].role == UnitRole::Body).collect();
    if body.is_empty() {
        return out;
    }
    // the item reading
    let mut item_words: Vec<Option<usize>> = vec![None; body.len()];
    let mut docs: Vec<(String, (usize, usize))> = Vec::new();
    if let Some(it) = item {
        let Some(me) = it.edits.get(it.index) else {
            out.inconsistent = Some("the item document does not name the child");
            return out;
        };
        if it.doc.get(me.fin.0..me.fin.1) != Some(t.text.as_str()) || me.units != t.paragraphs {
            out.inconsistent = Some("the item document does not hold the child's re-derived text and units");
            return out;
        }
        let placed: Vec<Placed> = it.edits.iter().map(|e| Placed { span: e.span, sep: e.sep.as_deref(), fin: e.fin, units: &e.units }).collect();
        item_words = reading(it.parent, it.doc, &placed, me.fin, &t.paragraphs, &body, &mut out.not_one_paragraph);
        docs.push((it.doc.to_string(), me.fin));
    }
    // the edit alone in its parent
    let mut alone_words: Vec<Option<usize>> = vec![None; body.len()];
    if let Some(o) = own
        && let Some((d, fin)) = alone_doc(o, &t.text)
    {
        let placed = [Placed { span: o.span, sep: o.sep, fin, units: &t.paragraphs }];
        alone_words = reading(o.parent, &d, &placed, fin, &t.paragraphs, &body, &mut out.not_one_paragraph);
        docs.push((d, fin));
    }
    if own.is_none() {
        docs.push((t.text.clone(), (0, t.text.len())));
    }
    let mut placed_any = false;
    for (k, &u) in body.iter().enumerate() {
        let (s, e) = t.paragraphs[u];
        let own_w = text::words(&t.text[s..e]);
        let mut arith = own_w;
        if src.mixed && u == 0 {
            arith += src.structure.pre_words;
        }
        if src.mixed && u + 1 == n {
            arith += src.structure.post_words;
        }
        let placed = (u > 0 && placed_edge(u - 1, u)) || (u + 1 < n && placed_edge(u, u + 1));
        placed_any |= placed;
        let (iw, aw) = (item_words[k], alone_words[k]);
        // a whole text's paragraph is the unit itself
        let short = if own.is_none() || (iw.is_none() && aw.is_none()) {
            own_w < MIN_PARAGRAPH_WORDS
        } else {
            iw.is_some_and(|w| w < MIN_PARAGRAPH_WORDS) || aw.is_some_and(|w| w < MIN_PARAGRAPH_WORDS)
        };
        let differs = own.is_some() && (iw.is_some_and(|w| w != arith) || aw.is_some_and(|w| w != arith));
        out.units.push(Unit6 { unit: u, placed, own: own_w, arithmetic: arith, item: iw, alone: aw, short, differs, remeasured_short: false });
    }
    // the re-measured reading, on the first assembled document
    if placed_any && let Some((d, fin)) = docs.first() {
        let m = Measure::of(d);
        for x in out.units.iter_mut().filter(|x| x.placed) {
            let (s, e) = t.paragraphs[x.unit];
            let r = m.paragraphs_overlapping(fin.0 + s, fin.0 + e);
            x.remeasured_short = m.paragraphs[r].iter().any(|p| text::words(&d[p.range.0..p.range.1]) < MIN_PARAGRAPH_WORDS);
        }
    }
    out
}

/// Whether a child's text-free per-request record shows that its
/// re-measured paragraphs differ from the constructed ones (item 31.3): the
/// record's `remeasure.differs`, its own edit's for an item record (the
/// edit at `item_check.index` of the last request's item check, else the
/// first, as index 60's `figures.py` reads it). None when the record holds
/// no such member.
pub fn remeasure_differs(rec: &serde_json::Value) -> Option<bool> {
    let r = match rec.get("edits").and_then(|e| e.as_array()).filter(|e| !e.is_empty()) {
        Some(eds) => {
            let ic = &rec["item_check"];
            let k = if ic["last"] == true { ic["index"].as_u64().unwrap_or(0) as usize } else { 0 };
            eds.get(k)?
        }
        None => rec,
    };
    r.get("remeasure")?.get("differs")?.as_bool()
}
