//! One child at a time (`Tally::count`): the re-derivation identity, then
//! the six counts, into cells keyed by source × class × domain (as
//! `DRY-RUN-FIGURES.json`'s `cells`), with reason maps by source × class and
//! an ids-only list for every child behind a non-zero stop invariant.
//!
//! **Identity.** A child is counted only when its landed transfer, re-run
//! on the recorded raw child, source and conventions, gives the recorded
//! laid-out text, and its landed verdict, recomputed through the batch's
//! wiring, gives the recorded acceptance and reasons. Otherwise it is
//! counted as a mismatch, with its ids, and nothing else is read from it;
//! any mismatch stops the run (`Tally::mismatched`).
//!
//! **Count 1** compares each child's recorded (landed) verdict with
//! amendment 7's, computed through the same wiring on the same raw child,
//! source and conventions:
//! * `accepted_to_rejected`: accepted under amendment 7's code and rejected
//!   under the landed code, counted by the landed code's reasons;
//! * `rejected_to_accepted`: the reverse, counted by amendment 7's reasons;
//! * `reasons_differ`: rejected under both with different reason sets,
//!   counted by the reasons only one code gives;
//! * `a7_failed`: amendment 7's code could not judge the child (a panic),
//!   counted by its failure code.
//!
//! **The gate.** The stop invariants gate on their broadest reading, so no
//! reading narrows item 14.1 (W7-R1): count 4 on accepted children, a line
//! over its budget under either reading; count 5 on every child; count 6 on
//! every child, the paragraph's own words. The narrower readings (accepted
//! children for counts 5 and 6, the final document's words for count 6) are
//! reported beside them.
use crate::engine::{self, Code, Failure, Laid, Source};
use crate::figures::{self, Context};
use crate::invariants::{self, Final};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

/// The landed verdict a child's record holds.
#[derive(Clone, Copy, Debug)]
pub struct Recorded<'a> {
    pub accepted: bool,
    pub reasons: &'a [String],
    /// The SHA-256 of the recorded laid-out child (a span's replacement):
    /// the output journal's text, or the profile record's `output_sha256`.
    pub text_sha256: &'a str,
}

/// Where a child sits in its final document (count 4's document reading).
#[derive(Clone, Copy, Debug)]
pub enum Fin<'a> {
    /// A whole text: the child is its final document.
    Whole,
    /// The recorded final document and the child's byte range in it.
    Doc(&'a str, (usize, usize)),
    /// The parent with the child's one span, spliced by `profile::splice`
    /// from the re-derived child; when the batch records the document's
    /// digest it must match. A splice that fails (a rejected child's) reads
    /// as `Unassembled`.
    Splice { parent: &'a str, span: (usize, usize), doc_sha256: Option<&'a str> },
    /// A span child whose final document does not assemble (a rejected
    /// child's): count 4's document reading reads the child alone, and the
    /// child is counted (`c4_children_without_final_document`).
    Unassembled,
}

/// One child to count.
pub struct Case<'a> {
    /// The batch label (for ids only).
    pub batch: &'a str,
    /// local, spliced, frontier, external_pool, stage3_legacy, hosted_capture.
    pub source: &'a str,
    /// The class (origin).
    pub class: &'a str,
    pub domain: &'a str,
    pub item_id: &'a str,
    pub request_id: Option<&'a str>,
    pub src: Source<'a>,
    pub raw: &'a str,
    /// The recorded conventions.
    pub conv: &'a Value,
    pub recorded: Recorded<'a>,
    /// The landed verdict recomputed through the batch's wiring.
    pub landed: Result<(bool, Vec<String>), Failure>,
    /// Amendment 7's verdict through the same wiring.
    pub a7: Result<(bool, Vec<String>), Failure>,
    /// The child's final document.
    pub fin: Fin<'a>,
}

/// A reason code safe for a record: codes are ids, but a reason built from
/// an error message is cut at its first `:` when it holds anything but
/// code characters.
pub fn safe_reason(r: &str) -> String {
    let ok = r.len() <= 160 && r.chars().all(|c| c.is_ascii_alphanumeric() || "_:.,/()<>=+- ".contains(c));
    if ok { r.to_string() } else { format!("{}:<other>", r.split(':').next().unwrap_or("").chars().filter(|c| c.is_ascii_alphanumeric() || *c == '_').take(60).collect::<String>()) }
}

fn sorted(v: &[String]) -> Vec<String> {
    let mut v: Vec<String> = v.iter().map(|r| safe_reason(r)).collect();
    v.sort();
    v.dedup();
    v
}

/// The counts of one batch.
#[derive(Clone, Debug, Default)]
pub struct Tally {
    /// `source|class|domain` -> figure -> n.
    pub cells: BTreeMap<String, BTreeMap<String, u64>>,
    /// `source|class` -> `figure:reason` -> n (count 1).
    pub reasons: BTreeMap<String, BTreeMap<String, u64>>,
    /// Ids-only lines (`IDS_SCHEMA`) of the children behind a non-zero stop
    /// invariant.
    pub ids: Vec<Value>,
    /// Mismatch reason -> n.
    pub mismatches: BTreeMap<String, u64>,
    /// Ids-only lines of the mismatched children.
    pub mismatch_ids: Vec<Value>,
}

fn bump(m: &mut BTreeMap<String, u64>, k: &str, n: u64) {
    if n > 0 {
        *m.entry(k.to_string()).or_default() += n;
    }
}

/// The text the parent holds on the child's first line before it (a span's
/// or an insertion's; empty for a whole text).
fn pre_line(src: Source) -> String {
    match src {
        Source::Span { parent, span } => parent.get(..span.0).map(|p| p[p.rfind('\n').map_or(0, |i| i + 1)..].to_string()).unwrap_or_default(),
        Source::Insertion { parent, at, sep } => match sep.rfind('\n') {
            Some(k) => sep[k + 1..].to_string(),
            None => parent.get(..at).map(|p| format!("{}{sep}", &p[p.rfind('\n').map_or(0, |i| i + 1)..])).unwrap_or_default(),
        },
        _ => String::new(),
    }
}

impl Tally {
    /// Whether any child mismatched (the run stops).
    pub fn mismatched(&self) -> u64 {
        self.mismatches.values().sum()
    }

    fn ids_line(case: &Case, what: &str, detail: Value) -> Value {
        json!({"schema": crate::IDS_SCHEMA, "batch": case.batch, "source": case.source, "class": case.class, "domain": case.domain,
            "item_id": case.item_id, "request_id": case.request_id, "kind": case.src.kind(), "accepted": case.recorded.accepted, "what": what, "detail": detail})
    }

    /// Counts one child (see the module notes).
    pub fn count(&mut self, case: &Case) {
        let key = format!("{}|{}|{}", case.source, case.class, case.domain);
        let acc = case.recorded.accepted;
        let landed = engine::landed_transfer(case.src, case.conv, case.raw);
        let rec_reasons = sorted(case.recorded.reasons);
        let mut mismatch: Option<String> = match &landed {
            Err(f) => Some(format!("landed_transfer_failed:{}", f.code())),
            Ok((_, t)) if crate::sha_hex(t.text.as_bytes()) != case.recorded.text_sha256 => Some("rederived_text_differs".into()),
            _ => None,
        };
        if mismatch.is_none() {
            mismatch = match &case.landed {
                Err(f) => Some(format!("landed_verdict_failed:{}", f.code())),
                Ok((a, r)) if *a != acc || sorted(r) != rec_reasons => Some("landed_verdict_differs".into()),
                _ => None,
            };
        }
        let mut inv = None;
        let mut unassembled = false;
        if mismatch.is_none()
            && let Ok((sl, t)) = &landed
        {
            let spliced;
            let alone = Final { doc: &t.text, fin: (0, t.text.len()) };
            let fin = match case.fin {
                Fin::Whole => Some(alone),
                Fin::Unassembled => {
                    unassembled = true;
                    Some(alone)
                }
                Fin::Doc(doc, fin) => Some(Final { doc, fin }),
                Fin::Splice { parent, span, doc_sha256 } => match engine::splice(Code::Landed, parent, span, &t.text) {
                    Ok(d) if doc_sha256.is_none_or(|h| crate::sha_hex(d.as_bytes()) == h) => {
                        spliced = d;
                        Some(Final { doc: &spliced, fin: (span.0, span.0 + t.text.len()) })
                    }
                    Ok(_) => {
                        mismatch = Some("final_document_differs".into());
                        None
                    }
                    Err(_) => {
                        unassembled = true;
                        Some(alone)
                    }
                },
            };
            if let Some(fin) = fin {
                let i = invariants::invariants(sl, t, Some(fin));
                if let Some(why) = i.inconsistent {
                    mismatch = Some(format!("diagnostics_inconsistent:{why}"));
                }
                inv = Some(i);
            }
        }
        if let Some(m) = mismatch {
            bump(&mut self.mismatches, &m, 1);
            bump(self.cells.entry(key).or_default(), "mismatch", 1);
            self.mismatch_ids.push(Self::ids_line(case, "mismatch", json!(m)));
            return;
        }
        let (sl, t) = landed.expect("identity checked");
        let inv = inv.expect("identity checked");
        let kind = case.src.kind();
        let rkey = format!("{}|{}", case.source, case.class);
        let mut c = std::mem::take(self.cells.entry(key.clone()).or_default());
        let mut r = std::mem::take(self.reasons.entry(rkey.clone()).or_default());
        bump(&mut c, "children", 1);
        bump(&mut c, "children_accepted", u64::from(acc));
        bump(&mut c, &format!("children|{kind}"), 1);
        // count 1
        match &case.a7 {
            Err(f) => {
                bump(&mut c, "c1_a7_failed", 1);
                bump(&mut r, &format!("c1_a7_failed:{}", f.code()), 1);
            }
            Ok((a7_acc, a7r)) => {
                let a7r = sorted(a7r);
                match (*a7_acc, acc) {
                    (true, true) => bump(&mut c, "c1_both_accepted", 1),
                    (true, false) => {
                        bump(&mut c, "c1_accepted_to_rejected", 1);
                        for x in &rec_reasons {
                            bump(&mut r, &format!("c1_accepted_to_rejected:{x}"), 1);
                        }
                    }
                    (false, true) => {
                        bump(&mut c, "c1_rejected_to_accepted", 1);
                        for x in &a7r {
                            bump(&mut r, &format!("c1_rejected_to_accepted:{x}"), 1);
                        }
                    }
                    (false, false) if a7r == rec_reasons => bump(&mut c, "c1_both_rejected_same_reasons", 1),
                    (false, false) => {
                        bump(&mut c, "c1_reasons_differ", 1);
                        let (lset, aset): (BTreeSet<&String>, BTreeSet<&String>) = (rec_reasons.iter().collect(), a7r.iter().collect());
                        for x in lset.difference(&aset) {
                            bump(&mut r, &format!("c1_reasons_differ:landed_only:{x}"), 1);
                        }
                        for x in aset.difference(&lset) {
                            bump(&mut r, &format!("c1_reasons_differ:a7_only:{x}"), 1);
                        }
                    }
                }
            }
        }
        // counts 2 and 3, under both codes
        let pre = pre_line(case.src);
        let ctx = Context {
            pre_line: &pre,
            pre_words: if sl.mixed { sl.structure.pre_words } else { 0 },
            post_words: if sl.mixed { sl.structure.post_words } else { 0 },
        };
        let lands: [(Code, Result<Laid, Failure>); 2] =
            [(Code::Landed, Ok(Laid { text: t.text.clone(), units: t.paragraphs.clone() })), (Code::A7, engine::laid(Code::A7, case.src, case.conv, case.raw))];
        for (code, l) in lands {
            let p = code.id();
            match l {
                Err(f) => {
                    bump(&mut c, &format!("c23_{p}_failed"), 1);
                    bump(&mut r, &format!("c23_{p}_failed:{}", f.code()), 1);
                }
                Ok(l) => {
                    let f = figures::form_figures(case.raw, &l, &ctx);
                    if f.fold_mismatch {
                        bump(&mut c, &format!("c23_{p}_fold_mismatch"), 1);
                        continue;
                    }
                    for (side, e) in [("greeting", f.greeting), ("sign_off", f.sign_off)] {
                        bump(&mut c, &format!("c2_{p}_{side}_children"), u64::from(e.present()));
                        bump(&mut c, &format!("c2_{p}_{side}_merged_children"), u64::from(e.merged()));
                        bump(&mut c, &format!("c2_{p}_{side}_merged_children|{kind}"), u64::from(e.merged()));
                        bump(&mut c, &format!("c2_{p}_{side}_lines"), e.lines as u64);
                        bump(&mut c, &format!("c2_{p}_{side}_lines_merged"), e.merged_lines as u64);
                        bump(&mut c, &format!("c2_{p}_{side}_lines_unanchored"), e.unanchored as u64);
                    }
                    bump(&mut c, &format!("c3_{p}_list_marker_lines"), f.list_marker_lines as u64);
                    bump(&mut c, &format!("c3_{p}_list_marker_lines_mid_line"), f.list_marker_mid_line as u64);
                    bump(&mut c, &format!("c3_{p}_list_marker_lines_mid_line|{kind}"), f.list_marker_mid_line as u64);
                    bump(&mut c, &format!("c3_{p}_children_with_list_marker_mid_line"), u64::from(f.list_marker_mid_line > 0));
                    bump(&mut c, &format!("c3_{p}_quoted_lines"), f.quoted_lines as u64);
                    bump(&mut c, &format!("c3_{p}_quoted_lines_mid_line"), f.quoted_mid_line as u64);
                    bump(&mut c, &format!("c3_{p}_list_lines_unanchored"), f.unanchored_list_lines as u64);
                }
            }
        }
        // counts 4-6 (landed)
        let a = u64::from(acc);
        let union: BTreeSet<(usize, usize)> = inv.over_budget.iter().chain(&inv.over_budget_document).map(|x| (x.unit, x.line)).collect();
        bump(&mut c, "c4_children_without_final_document", u64::from(unassembled));
        bump(&mut c, "c4_wrapped_lines", inv.wrapped_lines as u64);
        bump(&mut c, "c4_tail_lines", inv.tail_lines as u64);
        bump(&mut c, "c4_over_budget_lines", inv.over_budget.len() as u64);
        bump(&mut c, "c4_over_budget_lines_accepted", a * inv.over_budget.len() as u64);
        bump(&mut c, "c4_over_budget_tail_lines", inv.over_budget.iter().filter(|x| x.with_tail).count() as u64);
        bump(&mut c, "c4_over_budget_document_lines", inv.over_budget_document.len() as u64);
        bump(&mut c, "c4_over_budget_document_lines_accepted", a * inv.over_budget_document.len() as u64);
        bump(&mut c, "c4_gate_lines", a * union.len() as u64);
        bump(&mut c, "c4_gate_children", a * u64::from(!union.is_empty()));
        bump(&mut c, "c5_kept_units", inv.kept_units as u64);
        bump(&mut c, "c5_kept_list_lines", inv.kept_list_lines as u64);
        bump(&mut c, "c5_closed_units", inv.closed_units as u64);
        bump(&mut c, "c5_closed_split", inv.closed_split as u64);
        bump(&mut c, "c5_shared", inv.kept_shared.len() as u64);
        bump(&mut c, "c5_shared_accepted", a * inv.kept_shared.len() as u64);
        bump(&mut c, "c5_gate", inv.kept_shared.len() as u64);
        bump(&mut c, "c5_gate_children", u64::from(!inv.kept_shared.is_empty()));
        bump(&mut c, "c6_placed_paragraphs", inv.placed_paragraphs as u64);
        bump(&mut c, "c6_short_alone", inv.short_alone.len() as u64);
        bump(&mut c, "c6_short_alone_accepted", a * inv.short_alone.len() as u64);
        bump(&mut c, "c6_short_final", inv.short_final.len() as u64);
        bump(&mut c, "c6_short_final_accepted", a * inv.short_final.len() as u64);
        bump(&mut c, "c6_gate", inv.short_alone.len() as u64);
        bump(&mut c, "c6_gate_children", u64::from(!inv.short_alone.is_empty()));
        if acc && !union.is_empty() {
            let lines: Vec<Value> = union.iter().map(|(u, l)| json!([u, l])).collect();
            self.ids.push(Self::ids_line(case, "count4_wrapped_line_over_budget", json!({"lines": lines,
                "replacement_reading": inv.over_budget.len(), "document_reading": inv.over_budget_document.len()})));
        }
        if !inv.kept_shared.is_empty() {
            let kinds: Vec<Value> = inv.kept_shared.iter().map(|&k| json!([k, t.diag.kept[k].kind])).collect();
            self.ids.push(Self::ids_line(case, "count5_kept_unit_shares_a_unit", json!({"kept_units": kinds})));
        }
        if !inv.short_alone.is_empty() {
            self.ids.push(Self::ids_line(case, "count6_placed_break_paragraph_under_8_words", json!({"units": inv.short_alone,
                "final_document_reading_units": inv.short_final})));
        }
        *self.cells.get_mut(&key).expect("cell") = c;
        *self.reasons.get_mut(&rkey).expect("reasons") = r;
    }

    /// The batch's counts as JSON: cells, reasons, totals, the gate and the
    /// mismatches (no ids; those go to the ids-only file).
    pub fn to_json(&self) -> Value {
        let totals = totals(&self.cells);
        json!({"cells": self.cells, "reasons": self.reasons, "totals": totals, "gate": gate(&totals), "mismatches": self.mismatches,
            "mismatched_children": self.mismatched()})
    }
}

/// Totals of `cells` by source, by source × class, and over all.
pub fn totals(cells: &BTreeMap<String, BTreeMap<String, u64>>) -> BTreeMap<String, BTreeMap<String, u64>> {
    let mut out: BTreeMap<String, BTreeMap<String, u64>> = BTreeMap::new();
    for (k, c) in cells {
        let mut parts = k.splitn(3, '|');
        let (s, cl) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));
        for key in [s.to_string(), format!("{s}|{cl}"), "all".to_string()] {
            let t = out.entry(key).or_default();
            for (f, n) in c {
                *t.entry(f.clone()).or_default() += n;
            }
        }
    }
    out
}

/// The stop invariants' gating figures over `totals["all"]`: item 14.1's
/// three child-side invariants that index 60 did not count, at their
/// broadest readings, and whether every one is zero. Index 60's fourth (an
/// accepted child frame on a side where the source has none) was counted
/// there at 0 and is not re-counted here.
pub fn gate(totals: &BTreeMap<String, BTreeMap<String, u64>>) -> Value {
    let all = totals.get("all").cloned().unwrap_or_default();
    let g = |k: &str| all.get(k).copied().unwrap_or(0);
    let (c4, c5, c6) = (g("c4_gate_lines"), g("c5_gate"), g("c6_gate"));
    json!({
        "count4_accepted_wrapped_lines_over_budget": {"gate": c4, "children": g("c4_gate_children"),
            "replacement_reading_accepted": g("c4_over_budget_lines_accepted"), "document_reading_accepted": g("c4_over_budget_document_lines_accepted"),
            "every_child_replacement_reading": g("c4_over_budget_lines"), "every_child_document_reading": g("c4_over_budget_document_lines"), "population": "accepted children (items 10.12 and 13.10); a line counts when it is over its budget under either reading"},
        "count5_kept_unit_sharing_a_unit": {"gate": c5, "children": g("c5_gate_children"), "accepted": g("c5_shared_accepted"),
            "population": "every child; accepted children beside"},
        "count6_placed_break_paragraph_under_8_words": {"gate": c6, "children": g("c6_gate_children"), "accepted": g("c6_short_alone_accepted"),
            "final_document_reading": g("c6_short_final"), "final_document_reading_accepted": g("c6_short_final_accepted"),
            "population": "every child; the paragraph's own words gate, the final document's (with the parent words a span's first and last paragraphs join) beside"},
        "all_zero": c4 == 0 && c5 == 0 && c6 == 0,
        "zero_by_reading": {
            "count4": {"either_reading_accepted": c4 == 0, "replacement_reading_accepted": g("c4_over_budget_lines_accepted") == 0,
                "document_reading_accepted": g("c4_over_budget_document_lines_accepted") == 0},
            "count5": {"every_child": c5 == 0, "accepted": g("c5_shared_accepted") == 0},
            "count6": {"every_child_alone": c6 == 0, "every_child_final_document": g("c6_short_final") == 0, "accepted_alone": g("c6_short_alone_accepted") == 0,
                "accepted_final_document": g("c6_short_final_accepted") == 0}
        },
        "readings_note": "all_zero is the broadest readings' (W7-R1: no reading narrows item 14.1); a forward amendment may fix how an invariant is counted (BUILD-PLAN 12.3 open items (c), (d) and (g)), and zero_by_reading gives each reading's outcome",
        "index_60_counted": "an accepted child frame on a side where the source has none: 0 (log index 60)"
    })
}

/// Merges batch records (`BATCH_SCHEMA`): cells, reasons and mismatches
/// summed (a cell present in two batches adds), then totals and the gate.
pub fn merge(batches: &[Value]) -> Value {
    let mut cells: BTreeMap<String, BTreeMap<String, u64>> = BTreeMap::new();
    let mut reasons: BTreeMap<String, BTreeMap<String, u64>> = BTreeMap::new();
    let mut mismatches: BTreeMap<String, u64> = BTreeMap::new();
    let add = |into: &mut BTreeMap<String, BTreeMap<String, u64>>, from: &Value| {
        for (k, c) in from.as_object().into_iter().flatten() {
            let t = into.entry(k.clone()).or_default();
            for (f, n) in c.as_object().into_iter().flatten() {
                *t.entry(f.clone()).or_default() += n.as_u64().unwrap_or(0);
            }
        }
    };
    for b in batches {
        add(&mut cells, &b["counts"]["cells"]);
        add(&mut reasons, &b["counts"]["reasons"]);
        for (k, n) in b["counts"]["mismatches"].as_object().into_iter().flatten() {
            *mismatches.entry(k.clone()).or_default() += n.as_u64().unwrap_or(0);
        }
    }
    let totals = totals(&cells);
    let reasons_by_source = {
        let mut out: BTreeMap<String, BTreeMap<String, u64>> = BTreeMap::new();
        for (k, m) in &reasons {
            let s = k.split('|').next().unwrap_or("").to_string();
            for key in [s, "all".to_string()] {
                let t = out.entry(key).or_default();
                for (f, n) in m {
                    *t.entry(f.clone()).or_default() += n;
                }
            }
        }
        out
    };
    json!({"cells": cells, "reasons": reasons, "reasons_totals": reasons_by_source, "totals": totals, "gate": gate(&totals), "mismatches": mismatches,
        "mismatched_children": mismatches.values().sum::<u64>()})
}
