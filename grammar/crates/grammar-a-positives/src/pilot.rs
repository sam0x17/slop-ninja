//! Keyed pilot prefixes (B5b; protocol A2 §4.3 "Keyed pilot before bulk
//! generation", V22, S0-11, items 1.17 and 10.8).
//!
//! The pilot of each of D4–D8 is the smallest keyed prefix of the D-step's
//! plan in which every domain × class cell it touches holds at least 59
//! rows (R0's realised coverage floor); an item smaller than that is piloted
//! whole. A D-step's pilot and its bulk are one batch: the prior is the
//! whole plan's, written before the pilot (item 10.8), so the pilot is a
//! request order over the same plan, never a separate plan.
//!
//! **The rule** (`PILOT_RULE`; protocol A2 amendment 24, which fixes
//! what §4.3 and V22 leave open):
//! * **Rows.** The D-step's rows are its plan's planned items, one row per
//!   item: the items the given `generate --request-order` files list (D4's
//!   `d4-order-{GEN}.txt`, a `stratify-train` selection), or every item of
//!   the plan. A row is an item, not a request: an item with several
//!   requests yields one child.
//! * **Cells.** A row's cell is its item's domain × class, and its class is
//!   its origin, as the pilot gate reads its children (amendment 17 item
//!   17.1: "every child is a positive, whose class is its origin"):
//!   `model_only` for a mirror, `mixed` for every other kind (finalize's
//!   origin).
//! * **The key.** V21's plan key with x = `pilot` and d.id = the item id,
//!   `SHA-256("slop_ninja_a_comp_r2_plan_v1:pilot:" + item id)` (unkeyed: no
//!   A6 key; `span_ops::r2_key`), ascending, ties to the smaller item id.
//! * **The prefix.** The pilot is the shortest keyed prefix in which every
//!   cell the rows touch holds at least 59 rows. When a touched cell holds
//!   fewer than 59 rows in the whole D-step, no prefix reaches the floor
//!   there, the D-step is "smaller than that" (§4.3), and it is piloted
//!   whole: the pilot is every row, and the bulk is empty.
//!
//! The pilot and bulk order files keep the input order (a stratified order
//! stays stratified); the record names the prefix size, the last key, the
//! per-cell counts, the cells below the floor and every input digest, which
//! S0-11 records in `STAGE0.json`. D8's spliced plans are not piloted here
//! (their generator runs order windows).
use crate::split_copy::{OneSplit, new_dir, one_split, write_new};
use crate::{order, plan, span_ops, text};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::path::Path;

/// The pilot record (`PILOT.json`).
pub const PILOT_SCHEMA: &str = "slop_ninja_a_positives_pilot_v1";
/// V21's x of the pilot key.
pub const PILOT_KEY: &str = "pilot";
/// R0's realised coverage floor (V22; item 1.17), the pilot gate's floor.
pub const PILOT_FLOOR: usize = 59;
/// The prefix rule.
pub const PILOT_RULE: &str = "r2_pilot_prefix_v1 (protocol A2 section 4.3, V22, S0-11; amendment 24): the D-step's rows are its planned items, one per item (the items its request-order files list, or every item of the plan), in ascending V21 plan key SHA-256('slop_ninja_a_comp_r2_plan_v1:pilot:' + item id) (x = pilot, d.id = the item id, unkeyed: no A6 key), ties to the smaller item id; a row's cell is domain x class with class its origin (amendment 17 item 17.1; model_only for a mirror, mixed otherwise); the pilot is the shortest prefix in which every cell the rows touch holds at least 59 rows; when a touched cell holds fewer than 59 rows in the whole D-step, the D-step is piloted whole (section 4.3: 'an item smaller than that is piloted whole'); pilot and bulk are one batch under the plan's one prior (item 10.8), written as generate --request-order files in the input order";

/// One row of a D-step.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    pub item_id: String,
    pub generator: String,
    pub domain: String,
    pub origin: &'static str,
}

/// A row's class: its origin as `finalize` assigns it.
pub fn origin_of(kind: &str) -> &'static str {
    if kind == "mirror" { "model_only" } else { "mixed" }
}

/// The pilot key of an item.
pub fn pilot_key(item_id: &str) -> String {
    span_ops::r2_key(PILOT_KEY, item_id)
}

/// The pilot: the rows of the shortest keyed prefix ([`PILOT_RULE`]), as a
/// set of item ids, with the prefix length and its last key. The prefix
/// grows until every touched cell holds [`PILOT_FLOOR`] rows; a cell with
/// fewer rows in the whole D-step never gets there, so the pilot is then
/// every row (the D-step piloted whole).
pub fn prefix(rows: &[Row]) -> (BTreeSet<String>, usize, Option<String>) {
    let mut keyed: Vec<(String, &Row)> = rows.iter().map(|r| (pilot_key(&r.item_id), r)).collect();
    keyed.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.item_id.cmp(&b.1.item_id)));
    let cells: BTreeSet<(&str, &str)> = rows.iter().map(|r| (r.domain.as_str(), r.origin)).collect();
    // touched cells still below the floor in the prefix
    let mut short = cells.len();
    let mut have: BTreeMap<(&str, &str), usize> = BTreeMap::new();
    let mut out = BTreeSet::new();
    let mut last = None;
    for (k, r) in &keyed {
        if short == 0 {
            break;
        }
        out.insert(r.item_id.clone());
        last = Some(k.clone());
        let h = have.entry((r.domain.as_str(), r.origin)).or_default();
        *h += 1;
        if *h == PILOT_FLOOR {
            short -= 1;
        }
    }
    let n = out.len();
    (out, n, last)
}

/// The touched cells below the floor (`domain|origin`), which make the
/// D-step its own pilot.
pub fn cells_below_floor(rows: &[Row]) -> Vec<String> {
    let mut size: BTreeMap<String, usize> = BTreeMap::new();
    for r in rows {
        *size.entry(format!("{}|{}", r.domain, r.origin)).or_default() += 1;
    }
    size.into_iter().filter(|(_, n)| *n < PILOT_FLOOR).map(|(c, _)| c).collect()
}

/// Writes the pilot of the D-step on plan `pdir` to the new directory `out`:
/// `pilot-order-<generator>.txt`, `bulk-order-<generator>.txt` and
/// `PILOT.json`. `order` is a `generate --request-order` path with `{GEN}`
/// (a generator without such a file has no row), or none for every item.
pub fn run(pdir: &Path, order_tpl: Option<&str>, step: &str, out: &Path) -> Result<Value> {
    ensure!(!step.is_empty() && step.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-'), "step {step:?}: a D-step name such as D4");
    ensure!(!out.exists(), "{} exists: the pilot is a new directory", out.display());
    let pj = fs::read(pdir.join("plan.json"))?;
    let meta: Value = serde_json::from_slice(&pj)?;
    ensure!(meta["schema"] != crate::splice::PLAN_SCHEMA, "{} is a spliced plan: its generator runs order windows (D8's pilot is not built here)", pdir.display());
    let ib = fs::read(pdir.join("items.jsonl"))?;
    ensure!(meta["items_sha256"] == text::sha_hex(&ib).as_str(), "items.jsonl changed since planning");
    let split = one_split(&ib, "split", "items.jsonl", OneSplit::Rows)?.context("the plan has no item")?;
    let mut items: Vec<plan::Item> = Vec::new();
    for (n, l) in ib.split(|&b| b == b'\n').enumerate() {
        if !l.iter().all(u8::is_ascii_whitespace) {
            items.push(serde_json::from_slice(l).with_context(|| format!("items.jsonl line {}", n + 1))?);
        }
    }
    let by_id: HashMap<&str, &plan::Item> = items.iter().map(|i| (i.item_id.as_str(), i)).collect();
    ensure!(by_id.len() == items.len(), "duplicate item id");
    // the D-step's rows per generator, in input order
    let gens: Vec<String> = meta["request_files"].as_object().context("plan.json request_files")?.keys().cloned().collect();
    let mut lists: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut order_files = serde_json::Map::new();
    match order_tpl {
        Some(t) => {
            ensure!(t.contains(order::GEN_PLACEHOLDER), "the order path must hold {}", order::GEN_PLACEHOLDER);
            for g in &gens {
                let p = t.replace(order::GEN_PLACEHOLDER, g);
                let p = Path::new(&p);
                if !p.exists() {
                    continue;
                }
                let b = fs::read(p)?;
                let ids = order::parse_order(std::str::from_utf8(&b)?)?;
                for id in &ids {
                    let it = by_id.get(id.as_str()).with_context(|| format!("{}: item {id} is not in the plan", p.display()))?;
                    ensure!(it.generator == *g, "{}: item {id} is {}'s, not {g}'s", p.display(), it.generator);
                }
                order_files.insert(g.clone(), json!({"path": p, "sha256": text::sha_hex(&b), "items": ids.len()}));
                lists.insert(g.clone(), ids);
            }
            ensure!(!lists.is_empty(), "no order file matches {t}");
        }
        None => {
            for it in &items {
                lists.entry(it.generator.clone()).or_default().push(it.item_id.clone());
            }
        }
    }
    let mut rows: Vec<Row> = Vec::new();
    let mut seen: BTreeSet<&str> = BTreeSet::new();
    for ids in lists.values() {
        for id in ids {
            ensure!(seen.insert(id.as_str()), "item {id} is listed twice");
            let it = by_id[id.as_str()];
            rows.push(Row { item_id: id.clone(), generator: it.generator.clone(), domain: it.domain.clone(), origin: origin_of(&it.kind) });
        }
    }
    ensure!(!rows.is_empty(), "the D-step has no row");
    let (pilot, k, last) = prefix(&rows);
    // cells
    let mut cells: BTreeMap<String, (usize, usize)> = BTreeMap::new();
    for r in &rows {
        let c = cells.entry(format!("{}|{}", r.domain, r.origin)).or_default();
        c.0 += 1;
        c.1 += usize::from(pilot.contains(&r.item_id));
    }
    let cells_rec: serde_json::Map<String, Value> = cells
        .iter()
        .map(|(c, (n, p))| (c.clone(), json!({"rows": n, "pilot": p, "bulk": n - p, "at_floor": *n >= PILOT_FLOOR})))
        .collect();
    new_dir(out)?;
    let mut files = serde_json::Map::new();
    for (g, ids) in &lists {
        let (pl, bu): (Vec<&String>, Vec<&String>) = ids.iter().partition(|id| pilot.contains(*id));
        let mut rec = json!({"rows": ids.len(), "pilot": pl.len(), "bulk": bu.len()});
        for (name, part) in [("pilot", pl), ("bulk", bu)] {
            if part.is_empty() {
                continue;
            }
            let f = format!("{name}-order-{g}.txt");
            let b: String = part.iter().map(|i| format!("{i}\n")).collect();
            rec[name] = json!({"path": f, "sha256": write_new(&out.join(&f), b.as_bytes())?, "items": part.len()});
        }
        files.insert(g.clone(), rec);
    }
    let summary = json!({"schema": PILOT_SCHEMA, "code": crate::CODE_ID, "rule": PILOT_RULE, "step": step, "split": split,
        "key": {"x": PILOT_KEY, "prefix": span_ops::R2_PLAN_KEY_PREFIX, "d_id": "item id", "a6_key": false}, "floor": PILOT_FLOOR,
        "plan": {"dir": pdir, "plan_sha256": text::sha_hex(&pj), "items_sha256": meta["items_sha256"]},
        "order_files": order_files, "rows": rows.len(), "pilot_rows": k, "bulk_rows": rows.len() - k, "whole_step_is_pilot": k == rows.len(),
        "last_pilot_key": last, "cells": cells_rec, "cells_below_floor": cells_below_floor(&rows), "by_generator": files,
        "rule_source": "protocol A2 amendment 24 (rows, cells, key and the whole-step rule); section 4.3, V22 and amendment 17 item 17.1",
        "batch": "pilot and bulk are one batch: generate --profile --plan <plan> --request-order <this>/pilot-order-{GEN}.txt, then bulk-order-{GEN}.txt, under the plan's one prior (item 10.8)"});
    let mut b = serde_json::to_vec_pretty(&summary)?;
    b.push(b'\n');
    let h = write_new(&out.join("PILOT.json"), &b)?;
    let mut s = summary;
    s["pilot_sha256"] = json!(h);
    Ok(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows(spec: &[(&str, &'static str, usize)]) -> Vec<Row> {
        let mut v = Vec::new();
        for (d, o, n) in spec {
            for i in 0..*n {
                v.push(Row { item_id: format!("{d}-{o}-{i}"), generator: "g".into(), domain: d.to_string(), origin: o });
            }
        }
        v
    }

    /// The prefix rule checked independently of `prefix`: the whole D-step
    /// when a cell holds fewer than 59 rows, else the shortest k such that
    /// the first k keyed rows hold 59 rows of every cell.
    fn brute(rs: &[Row]) -> usize {
        let mut keyed: Vec<(String, &Row)> = rs.iter().map(|r| (text::sha_hex(format!("slop_ninja_a_comp_r2_plan_v1:pilot:{}", r.item_id).as_bytes()), r)).collect();
        keyed.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.item_id.cmp(&b.1.item_id)));
        let cells: BTreeSet<(String, &str)> = rs.iter().map(|r| (r.domain.clone(), r.origin)).collect();
        if cells.iter().any(|(d, o)| rs.iter().filter(|r| r.domain == *d && r.origin == *o).count() < 59) {
            return rs.len();
        }
        for k in 0..=keyed.len() {
            if cells.iter().all(|(d, o)| keyed[..k].iter().filter(|(_, r)| r.domain == *d && r.origin == *o).count() >= 59) {
                return k;
            }
        }
        unreachable!()
    }

    #[test]
    fn the_prefix_is_the_shortest_keyed_prefix_meeting_every_cell() {
        assert_eq!(pilot_key("x"), text::sha_hex(b"slop_ninja_a_comp_r2_plan_v1:pilot:x"));
        for spec in [
            vec![("wiki", "mixed", 300), ("wiki", "model_only", 200), ("legal", "mixed", 150)],
            vec![("wiki", "mixed", 59), ("legal", "model_only", 400)],
            vec![("wiki", "mixed", 60), ("legal", "model_only", 61), ("news", "mixed", 1000)],
        ] {
            let rs = rows(&spec);
            let (p, k, last) = prefix(&rs);
            assert_eq!(k, brute(&rs), "{spec:?}");
            assert_eq!(p.len(), k);
            assert!(cells_below_floor(&rs).is_empty());
            // every cell holds at least 59 pilot rows, and the last key is the prefix's largest
            for (d, o, _) in &spec {
                assert!(rs.iter().filter(|r| r.domain == *d && r.origin == *o && p.contains(&r.item_id)).count() >= 59);
            }
            let maxk = rs.iter().filter(|r| p.contains(&r.item_id)).map(|r| pilot_key(&r.item_id)).max();
            assert_eq!(last, maxk);
            // every row outside the pilot has a larger key than every pilot row
            let lastk = last.unwrap();
            assert!(rs.iter().filter(|r| !p.contains(&r.item_id)).all(|r| pilot_key(&r.item_id) > lastk));
        }
        // a cell of exactly 59 rows needs every one of them; with a large cell beside it the bulk is not empty
        let rs = rows(&[("wiki", "mixed", 59), ("news", "mixed", 2000)]);
        let (p, k, _) = prefix(&rs);
        assert_eq!(k, brute(&rs));
        assert_eq!(rs.iter().filter(|r| r.domain == "wiki" && p.contains(&r.item_id)).count(), 59);
        assert!(k < rs.len());
    }

    #[test]
    fn a_step_with_a_cell_below_the_floor_is_piloted_whole() {
        // amendment 24: 12 legal mixed rows can never hold 59, so the whole D-step is its pilot, the
        // 500-row cell's bulk included (section 4.3: an item smaller than that is piloted whole)
        let rs = rows(&[("legal", "mixed", 12), ("wiki", "model_only", 500)]);
        let (p, k, last) = prefix(&rs);
        assert_eq!((k, p.len(), k), (512, 512, brute(&rs)));
        assert_eq!(cells_below_floor(&rs), ["legal|mixed"]);
        assert_eq!(last, rs.iter().map(|r| pilot_key(&r.item_id)).max());
        // 58 rows in one cell: still whole
        let rs = rows(&[("legal", "mixed", 58), ("wiki", "model_only", 900)]);
        assert_eq!(prefix(&rs).1, 958);
        // a D-step below the floor everywhere is its own pilot
        let small = rows(&[("legal", "mixed", 40), ("legal", "model_only", 3)]);
        let (p, k, _) = prefix(&small);
        assert_eq!((k, p.len()), (43, 43));
        assert_eq!(prefix(&[]).1, 0);
    }

    #[test]
    fn origin_is_finalizes() {
        assert_eq!(origin_of("mirror"), "model_only");
        for k in ["span", "full", "continuation", "splice", "prefix_continue"] {
            assert_eq!(origin_of(k), "mixed");
        }
    }
}
