//! D4's plan: renorm's `regenerate_mirror` rows as new requests (B5b; ruling
//! R6 of BUILD-PLAN section 10.1; protocol A2 §6.3, D4 and amendment 20 item
//! 20.5).
//!
//! A mirror that fails profile v2 at the §4.3 re-verdict is regenerated
//! through a **new request** asking for `mirror_target_words_v2` words
//! (`plan::mirror_target_words_v2`), with at most its remaining attempts,
//! counted from the first (`Request::attempt_limit`). It is not resumed: a
//! resumed request keeps its original target.
//!
//! **The plan** (`mirror_plan`, `slop_ninja_a_positives mirror-plan`):
//! * Its base is a plan whose files hold the one split of renorm's batch (a
//!   one-split copy, `split_copy`), and renorm's output directory for that
//!   split: `RENORM-split-<split>.json`, the regeneration plan and the
//!   `regenerate-order-<generator>.txt` files, each checked against the
//!   summary's digests. Renorm must have read the base plan or its copy's
//!   source (by `plan.json` digest), and its items, request files and input
//!   must be the base's.
//! * Every line of the base is copied byte for byte except each
//!   `regenerate_mirror` item, whose one request is replaced in place (the
//!   item keeps its id, parent, generator, decoding and cohort, so its
//!   keyed form draws are the same). The new request
//!   (`req-` + SHA-256(old request id `:mirror_target_words_v2`)[..24]) asks
//!   for `mirror_target_words_v2` words: the prompt's one
//!   ` in English, about N words long.` with N the old target, which must be
//!   the parent's words (R0's mirrors were requested at the parent's length),
//!   becomes the new target; `target_words` and the request digest (under
//!   the template string that reproduces the old request's digest) follow;
//!   `attempt_limit` is the row's `attempts_remaining`, which must be at
//!   least 1 and the row's limit less its used attempts.
//! * **The seed** (protocol A2 amendment 50 item 50.1). A lifted mirror (a
//!   new target, so a new prompt and digest) takes its seed from its digest
//!   and runs its attempts from 0: no earlier attempt has its body. A mirror
//!   whose target does not change has the old request's prompt and digest,
//!   so it continues the old request's attempt index: its seed is the old
//!   seed plus the attempts used (`plan::attempt_seed`), and its first
//!   attempt (`generate` starts a request without a journal at 0) is sent at
//!   s + k for the item's attempt index k, never at an earlier attempt's
//!   seed. For R0's two-attempt rows that is s + 1.
//! * **The batch** ([`BATCH_READING`]). D4 is one batch with one prior (item
//!   10.8; "Train-2 gets one prior per batch (D4, D5, D7)"), and renorm wrote
//!   that batch's binding and prior for D4's resumes. The new mirrors are D4's,
//!   so they are judged in the same batch: the plan's items keep the base's
//!   parents, so `generate --profile` builds the same prior and batch record.
//!   The plan checks that before it writes anything, by building the step
//!   (`FormPost`) from its split's input and comparing its record with
//!   renorm's.
//! * **D4's order files** (`d4-order-<generator>.txt`): the `regenerate`
//!   items (renorm's order files) and the regenerated mirrors, in the plan's
//!   item order. D4 is `generate --profile ... --plan <this> --runs <renorm
//!   output> --request-order <this>/d4-order-{GEN}.txt` (its pilot first,
//!   `pilot`).
//! * `drop_out_of_attempts` rows get no request; they are counted.
//! * **r05** is read from `VALUES-R2.json`'s S0-6 entry (`crate::values`),
//!   never from the command line; the plan refuses while S0-6 is absent
//!   (protocol A2 §6.3: frozen at A9 before any D4 mirror), and its record
//!   names the register's digest and the value read.
//! * **The overlap screen** (amendment 24): the base's inputs are kept, so
//!   a split copy's CAL overlap extra (`split_copy::OVERLAP_READING`) carries
//!   over to D4's plan.
use crate::plan::{self, Item, MIRROR_TARGET_V2_ID, R05, Request};
use crate::renorm::{REGEN_SCHEMA, RENORM_SCHEMA};
use crate::split_copy::{OneSplit, new_dir, one_split, write_new};
use crate::{form_post, text};
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::path::{Path, PathBuf};

/// The `mirror_regeneration` record of the plan's `plan.json`.
pub const MIRROR_REGEN_SCHEMA: &str = "slop_ninja_a_positives_mirror_regeneration_v2";
/// One line of `mirror-regeneration.jsonl` (ids and counts only).
pub const MIRROR_REGEN_ROW_SCHEMA: &str = "slop_ninja_a_positives_mirror_regeneration_row_v2";
/// The note a regenerated mirror item gains.
pub const MIRROR_REGEN_NOTE: &str = "regenerated_mirror:mirror_target_words_v2";

/// How a regenerated mirror's request is made.
pub const MIRROR_REGEN_RULE: &str = "each regenerate_mirror row of renorm's regeneration plan becomes a new request of the same item (ruling R6; amendment 20 item 20.5): request id req- + SHA-256(old request id + ':mirror_target_words_v2')[..24]; the prompt's one ' in English, about N words long.' (N the old target, which must equal the parent's words) asks for mirror_target_words_v2 words, target_words follows, the request digest is taken under the template string that reproduces the old request's digest; the seed follows R0's attempt rule (protocol A2 amendment 50 item 50.1: the item's attempt at index k is sent at seed s + k, u32 wrapping, and no attempt reuses an earlier attempt's seed): a lifted mirror (new target, so a new prompt and digest) takes the seed from its digest and its attempts from 0, while a mirror whose target does not change (the old prompt and digest) continues the old request's attempt index, its seed being the old seed plus the row's attempts_used, so R0's two-attempt rows send their one remaining attempt at s + 1, not s; attempt_limit is the row's attempts_remaining (>= 1, the row's limit less its used attempts), so attempts stop at the inherited per-item limit counted from the first; not a resume: a resumed request keeps its original target";

/// Which batch, binding and prior judge the new mirror requests (BUILD-PLAN
/// 10.4's open question, read from the text).
pub const BATCH_READING: &str = "the new mirror requests are D4's and D4 is one batch with one prior (amendment 10 item 10.8: 'Train-2 gets one prior per batch (D4, D5, D7)'; a D-step's pilot and bulk are one batch); renorm's output binding and prior are that batch's (renorm: 'the output runs directory resumes under generate --profile (D4) with the same record'); so the plan keeps every item and parent of renorm's batch, generate --profile builds the same prior and record, and the new requests run on renorm's output runs; the plan checks the record before writing";

fn sha(b: &[u8]) -> String {
    text::sha_hex(b)
}

fn read_json(p: &Path) -> Result<Value> {
    serde_json::from_slice(&fs::read(p).with_context(|| p.display().to_string())?).with_context(|| p.display().to_string())
}

/// The lines of a one-split JSONL file (`one_split` first), parsed.
fn one_split_lines(b: &[u8], what: &str, split: &str, kind: OneSplit) -> Result<Vec<Value>> {
    if let Some(s) = one_split(b, "split", what, kind)? {
        ensure!(s == split, "{what} holds split {s}, not {split}");
    }
    let mut v = Vec::new();
    for (n, l) in b.split(|&x| x == b'\n').enumerate() {
        if !l.iter().all(u8::is_ascii_whitespace) {
            v.push(serde_json::from_slice(l).with_context(|| format!("{what} line {}", n + 1))?);
        }
    }
    Ok(v)
}

/// The template string under which `r`'s digest was taken: plan v1 and v3
/// hash the template id, plan v2 `<template id>:<operation or kind>`.
fn digest_template(r: &Request, kind: &str) -> Result<String> {
    let mut cands = vec![r.template_id.clone(), format!("{}:{kind}", r.template_id)];
    if let Some(op) = &r.op {
        cands.push(format!("{}:{op}", r.template_id));
    }
    for t in cands {
        if plan::request_sha(&r.generator, &r.endpoint, &t, &r.prompt, &r.decoding) == r.request_sha256 {
            return Ok(t);
        }
    }
    bail!("request {}: no template string reproduces its recorded digest", r.request_id)
}

/// The new request of a regenerated mirror ([`MIRROR_REGEN_RULE`]), after
/// `attempts_used` of the old request's attempts, with `attempts_remaining`
/// left (together the old request's limit).
pub fn new_mirror_request(old: &Request, kind: &str, parent_words: usize, attempts_used: u32, attempts_remaining: u32, r05: &R05) -> Result<Request> {
    ensure!(old.endpoint == "chat" && old.source_span.is_none(), "request {}: not a mirror request", old.request_id);
    ensure!(old.target_words == parent_words, "request {}: its target ({}) is not its parent's words ({parent_words}); R0's mirrors were requested at the parent's length", old.request_id, old.target_words);
    ensure!(attempts_remaining >= 1, "request {}: no attempt remains (a drop_out_of_attempts item gets no request)", old.request_id);
    ensure!(attempts_used.checked_add(attempts_remaining) == Some(old.attempts()),
        "request {}: attempts used {attempts_used} and remaining {attempts_remaining} do not make its limit {}", old.request_id, old.attempts());
    let target = plan::mirror_target_words_v2(parent_words, r05);
    let from = format!(" in English, about {} words long.", old.target_words);
    ensure!(old.prompt.matches(&from).count() == 1, "request {}: its prompt does not state its length exactly once", old.request_id);
    let prompt = old.prompt.replacen(&from, &format!(" in English, about {target} words long."), 1);
    let t = digest_template(old, kind)?;
    let rs = plan::request_sha(&old.generator, &old.endpoint, &t, &prompt, &old.decoding);
    // amendment 50 item 50.1: an unchanged target keeps the old body, so the
    // item's attempt index continues (seed s + attempts used); a lifted
    // mirror is a new body with its digest's seed and attempts from 0
    let seed = if target == old.target_words {
        ensure!(prompt == old.prompt && rs == old.request_sha256, "request {}: an unchanged target did not keep its prompt and digest", old.request_id);
        plan::attempt_seed(old.seed, attempts_used)
    } else {
        ensure!(rs != old.request_sha256, "request {}: a new target kept its digest", old.request_id);
        plan::seed_of(&rs)
    };
    Ok(Request {
        request_id: format!("req-{}", &sha(format!("{}:{MIRROR_TARGET_V2_ID}", old.request_id).as_bytes())[..24]),
        prompt,
        target_words: target,
        seed,
        request_sha256: rs,
        attempt_limit: Some(attempts_remaining),
        ..old.clone()
    })
}

/// Writes D4's plan (see the module notes) to the new directory `out`, from
/// base plan `base` and renorm's output directory `renorm` for split
/// `split`, with r05 read from `values` (S0-6). Returns the plan's
/// `mirror_regeneration` record.
pub fn mirror_plan(base: &Path, renorm: &Path, split: &str, values: &crate::values::Values, out: &Path) -> Result<Value> {
    ensure!(!crate::renorm::refused_split(split), "split {split} is refused (protocol A2 section 2.4)");
    ensure!(!out.exists(), "{} exists: the plan is a new directory", out.display());
    let (r05, values_rec) = crate::values::r05(values)?;
    let r05 = &r05;
    // renorm's summary and outputs
    let batch = format!("split-{split}");
    let sum_p = renorm.join(format!("RENORM-{batch}.json"));
    let sum = read_json(&sum_p)?;
    ensure!(sum["schema"] == RENORM_SCHEMA && sum["plan_kind"] == "local" && sum["split"] == split && sum["batch"] == batch.as_str(),
        "{}: not renorm's local summary of split {split}", sum_p.display());
    let rg = &sum["outputs"]["regeneration_plan"];
    let rg_b = fs::read(renorm.join(rg["path"].as_str().context("regeneration plan path")?))?;
    ensure!(rg["sha256"] == sha(&rg_b).as_str(), "the regeneration plan differs from renorm's summary");
    let mut renorm_orders: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (g, o) in sum["outputs"]["request_orders"].as_object().context("request orders")? {
        let b = fs::read(renorm.join(o["path"].as_str().context("order path")?))?;
        ensure!(o["sha256"] == sha(&b).as_str(), "renorm's order file of {g} differs from its summary");
        renorm_orders.insert(g.clone(), crate::order::parse_order(std::str::from_utf8(&b)?)?);
    }
    // the base: one split, renorm's plan or its copy's source
    let pj = fs::read(base.join("plan.json"))?;
    let meta: Value = serde_json::from_slice(&pj)?;
    ensure!(meta["schema"] != crate::splice::PLAN_SCHEMA, "{} is a spliced plan", base.display());
    let source_sha = meta["split_copy"]["source"]["plan_sha256"].as_str();
    ensure!(sum["inputs"]["plan_sha256"] == sha(&pj).as_str() || source_sha.is_some_and(|s| sum["inputs"]["plan_sha256"] == s),
        "renorm read another plan than the base or its copy's source");
    let items_b = fs::read(base.join("items.jsonl"))?;
    ensure!(meta["items_sha256"] == sha(&items_b).as_str(), "items.jsonl changed since planning");
    let items: Vec<Value> = one_split_lines(&items_b, "items.jsonl", split, OneSplit::Rows)?;
    let fails_b = fs::read(base.join("plan-failures.jsonl"))?;
    ensure!(meta["plan_failures_sha256"] == sha(&fails_b).as_str(), "plan-failures.jsonl changed since planning");
    one_split_lines(&fails_b, "plan-failures.jsonl", split, OneSplit::PlanFailures)?;
    let mut req_b: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    let mut requests: HashMap<String, Request> = HashMap::new();
    for (g, rec) in meta["request_files"].as_object().context("plan.json request_files")? {
        let b = fs::read(base.join("requests").join(format!("{g}.jsonl")))?;
        ensure!(rec["sha256"] == sha(&b).as_str(), "request file {g} changed since planning");
        for v in one_split_lines(&b, &format!("requests/{g}.jsonl"), split, OneSplit::Rows)? {
            let r: Request = serde_json::from_value(v)?;
            ensure!(r.generator == *g, "request {} is in {g}'s file", r.request_id);
            ensure!(requests.insert(r.request_id.clone(), r).is_none(), "duplicate request id");
        }
        req_b.insert(g.clone(), b);
    }
    let inp = &meta["inputs"][split];
    let ip = PathBuf::from(inp["path"].as_str().context("input path")?);
    let ib = fs::read(&ip)?;
    ensure!(inp["sha256"] == sha(&ib).as_str(), "input {split} changed since planning");
    ensure!(sum["inputs"]["input"]["sha256"] == inp["sha256"], "renorm read another {split} input");
    // the items and parents
    let mut by_item: HashMap<String, Item> = HashMap::new();
    let mut parent_ids: BTreeSet<String> = BTreeSet::new();
    for v in &items {
        let it: Item = serde_json::from_value(v.clone())?;
        parent_ids.insert(it.parent_id.clone());
        ensure!(by_item.insert(it.item_id.clone(), it).is_none(), "duplicate item id");
    }
    let mut parents: Vec<plan::Human> = Vec::new();
    for v in one_split_lines_key(&ib, "frozen_split", split, "input")? {
        let h = plan::human_from(&v, split)?;
        if parent_ids.contains(&h.id) {
            parents.push(h);
        }
    }
    ensure!(parents.len() == parent_ids.len(), "split {split}: {} item parents, {} rows of its input match them", parent_ids.len(), parents.len());
    // the batch: the same step and record as renorm's
    let pop = sum["profile"]["population"].as_str().context("renorm's population")?;
    let fp = form_post::FormPost::new(pop, parents.iter().map(|h| (h.id.as_str(), h.domain, h.text.as_str())))?;
    ensure!(fp.record() == sum["profile"], "the plan's batch record differs from renorm's: the new mirrors would not be judged in renorm's batch");
    let words: HashMap<&str, usize> = parents.iter().map(|h| (h.id.as_str(), h.word_count)).collect();
    // the regeneration plan's rows
    let rows = one_split_lines(&rg_b, "regeneration plan", split, OneSplit::Rows)?;
    let mut replace: HashMap<String, Request> = HashMap::new();
    let mut regen_items: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut by_action: BTreeMap<String, usize> = BTreeMap::new();
    let mut lines: Vec<Value> = Vec::new();
    let mut by_gen: BTreeMap<String, usize> = BTreeMap::new();
    let mut unchanged_target = 0usize;
    for r in &rows {
        ensure!(r["schema"] == REGEN_SCHEMA && r["batch"] == batch.as_str(), "a regeneration plan row of another schema or batch");
        let a = r["action"].as_str().context("action")?;
        *by_action.entry(a.to_string()).or_default() += 1;
        let rid = r["request_id"].as_str().context("request_id")?;
        let iid = r["item_id"].as_str().context("item_id")?;
        let it = by_item.get(iid).with_context(|| format!("regeneration row: item {iid} is not in the base"))?;
        let old = requests.get(rid).with_context(|| format!("regeneration row: request {rid} is not in the base"))?;
        ensure!(old.item_id == iid && it.requests.contains(&rid.to_string()) && r["generator"] == old.generator.as_str() && r["parent_id"] == it.parent_id.as_str()
            && r["kind"] == it.kind.as_str() && r["target_words"].as_u64() == Some(old.target_words as u64),
            "regeneration row of request {rid}: its item, generator, parent, kind or target differs from the base");
        match a {
            "regenerate" => {
                regen_items.entry(old.generator.clone()).or_default().insert(iid.to_string());
            }
            "drop_out_of_attempts" => {}
            "regenerate_mirror" => {
                ensure!(it.kind == "mirror" && it.requests.len() == 1, "regenerate_mirror row of request {rid}: item {iid} is not a one-request mirror");
                let used = r["attempts_used"].as_u64().context("attempts_used")?;
                let limit = r["attempt_limit"].as_u64().context("attempt_limit")?;
                let left = r["attempts_remaining"].as_u64().context("attempts_remaining")?;
                ensure!(limit == old.attempts() as u64 && left == limit.saturating_sub(used) && left >= 1,
                    "regenerate_mirror row of request {rid}: attempts used {used}, limit {limit}, remaining {left} do not follow the attempt rule");
                let pw = *words.get(it.parent_id.as_str()).context("parent words")?;
                let new = new_mirror_request(old, &it.kind, pw, used as u32, left as u32, r05)?;
                let continued = new.target_words == old.target_words;
                if continued {
                    unchanged_target += 1;
                }
                ensure!(!requests.contains_key(&new.request_id), "request id {} exists", new.request_id);
                *by_gen.entry(old.generator.clone()).or_default() += 1;
                lines.push(json!({"schema": MIRROR_REGEN_ROW_SCHEMA, "item_id": iid, "generator": old.generator, "old_request_id": rid,
                    "new_request_id": new.request_id, "parent_words": pw, "old_target_words": old.target_words, "new_target_words": new.target_words,
                    "attempts_used": used, "attempt_limit": limit, "attempts_remaining": left, "new_request_sha256": new.request_sha256,
                    "old_seed": old.seed, "new_seed": new.seed, "seed_rule": if continued { "attempt_index_continued" } else { "new_digest" }}));
                ensure!(replace.insert(rid.to_string(), new).is_none(), "request {rid} planned twice");
            }
            x => bail!("regeneration row of request {rid}: unknown action {x}"),
        }
    }
    // renorm's order files list exactly the regenerate rows' items
    for (g, ids) in &renorm_orders {
        let want = regen_items.get(g).cloned().unwrap_or_default();
        ensure!(ids.iter().cloned().collect::<BTreeSet<_>>() == want, "renorm's order file of {g} is not its regenerate rows' items");
    }
    ensure!(regen_items.keys().all(|g| renorm_orders.contains_key(g)), "a generator with regenerate rows has no renorm order file");
    // the new plan
    new_dir(out)?;
    new_dir(&out.join("requests"))?;
    let mirror_items: HashMap<&str, &Request> = replace.values().map(|r| (r.item_id.as_str(), r)).collect();
    let mut ib_out = Vec::new();
    for (l, v) in items_b.split(|&b| b == b'\n').filter(|l| !l.iter().all(u8::is_ascii_whitespace)).zip(&items) {
        let id = v["item_id"].as_str().context("item_id")?;
        match mirror_items.get(id) {
            Some(nr) => {
                let mut it: Item = serde_json::from_value(v.clone())?;
                it.requests = vec![nr.request_id.clone()];
                it.notes.push(MIRROR_REGEN_NOTE.into());
                serde_json::to_writer(&mut ib_out, &it)?;
            }
            None => ib_out.extend_from_slice(l),
        }
        ib_out.push(b'\n');
    }
    let items_sha = write_new(&out.join("items.jsonl"), &ib_out)?;
    let fails_sha = write_new(&out.join("plan-failures.jsonl"), &fails_b)?;
    let mut req_files = serde_json::Map::new();
    for (g, b) in &req_b {
        let mut o = Vec::new();
        let mut n = 0usize;
        for l in b.split(|&x| x == b'\n').filter(|l| !l.iter().all(u8::is_ascii_whitespace)) {
            let v: Value = serde_json::from_slice(l)?;
            match replace.get(v["request_id"].as_str().unwrap_or_default()) {
                Some(nr) => serde_json::to_writer(&mut o, nr)?,
                None => o.extend_from_slice(l),
            }
            o.push(b'\n');
            n += 1;
        }
        req_files.insert(g.clone(), json!({"requests": n, "sha256": write_new(&out.join("requests").join(format!("{g}.jsonl")), &o)?}));
    }
    let mut rows_b = Vec::new();
    for l in &lines {
        serde_json::to_writer(&mut rows_b, l)?;
        rows_b.push(b'\n');
    }
    let rows_sha = write_new(&out.join("mirror-regeneration.jsonl"), &rows_b)?;
    // D4's order files: regenerate items and regenerated mirrors, in item order
    let mut d4: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for v in &items {
        let id = v["item_id"].as_str().unwrap_or_default();
        let g = v["generator"].as_str().unwrap_or_default();
        if mirror_items.contains_key(id) || regen_items.get(g).is_some_and(|s| s.contains(id)) {
            d4.entry(g.to_string()).or_default().push(id.to_string());
        }
    }
    let mut orders = serde_json::Map::new();
    for (g, ids) in &d4 {
        let name = format!("d4-order-{g}.txt");
        let b: String = ids.iter().map(|i| format!("{i}\n")).collect();
        orders.insert(g.clone(), json!({"path": name, "sha256": write_new(&out.join(&name), b.as_bytes())?, "items": ids.len()}));
    }
    let rec = json!({"schema": MIRROR_REGEN_SCHEMA, "code": crate::CODE_ID, "rule": MIRROR_REGEN_RULE, "batch_reading": BATCH_READING,
        "mirror_target": r05.record(), "values": values_rec, "split": split, "batch": batch,
        "base": {"plan": base, "plan_sha256": sha(&pj), "items_sha256": meta["items_sha256"], "request_files": meta["request_files"]},
        "renorm": {"dir": renorm, "summary_sha256": sha(&fs::read(&sum_p)?), "regeneration_plan_sha256": rg["sha256"], "prior_sha256": sum["prior_sha256"],
            "population": pop},
        "profile_record_equals_renorm": true,
        "rows_by_action": by_action, "regenerated_mirrors": lines.len(), "regenerated_mirrors_by_generator": by_gen,
        "unchanged_target_mirrors": unchanged_target,
        "rows": {"path": "mirror-regeneration.jsonl", "sha256": rows_sha}, "d4_orders": orders,
        "d4": "generate --profile cc_cohort_profile_v2 --profile-population <population> --plan <this> --splits <split> --runs <renorm output> --request-order <this>/d4-order-{GEN}.txt (the keyed pilot's order files first)"});
    let mut m = meta.as_object().cloned().context("plan.json")?;
    m.insert("items_sha256".into(), json!(items_sha));
    m.insert("plan_failures_sha256".into(), json!(fails_sha));
    m.insert("request_files".into(), Value::Object(req_files));
    m.insert("mirror_regeneration".into(), rec.clone());
    let mut pb = serde_json::to_vec_pretty(&Value::Object(m))?;
    pb.push(b'\n');
    let plan_sha = write_new(&out.join("plan.json"), &pb)?;
    let mut r = rec;
    r["plan_sha256"] = json!(plan_sha);
    Ok(r)
}

/// Lines of `b` whose `key` names `split` (read from bytes; others skipped
/// unparsed), parsed.
fn one_split_lines_key(b: &[u8], key: &str, split: &str, what: &str) -> Result<Vec<Value>> {
    let mut sk = crate::split_copy::Skipped::new();
    let (sel, _) = crate::split_copy::select_lines(b, key, split, what, what, false, &mut sk)?;
    let mut v = Vec::new();
    for (n, l) in sel.split(|&x| x == b'\n').enumerate() {
        if !l.iter().all(u8::is_ascii_whitespace) {
            v.push(serde_json::from_slice(l).with_context(|| format!("{what} selected line {}", n + 1))?);
        }
    }
    Ok(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn old(template_digest: &str, target: usize) -> Request {
        let prompt = format!("Write a synthetic note in English, about {target} words long. Keep it plain.");
        let mut r: Request = serde_json::from_value(json!({"request_id": "req-old", "item_id": "it", "parent_id": "p", "split": "train", "generator": "g",
            "endpoint": "chat", "template_id": "mirror_v1", "prompt": prompt, "span_index": 0, "source_span": null, "target_words": target, "cut_budget": null,
            "decoding": {"profile": "A", "temperature": 0.7, "top_p": 0.9, "top_k": 0, "min_p": 0.0, "repeat_penalty": 1.0, "max_tokens": 512},
            "request_sha256": "", "seed": 0, "cohort": "c"}))
        .unwrap();
        r.request_sha256 = plan::request_sha(&r.generator, &r.endpoint, template_digest, &r.prompt, &r.decoding);
        r.seed = plan::seed_of(&r.request_sha256);
        r
    }

    /// The new request as the code before B31 made it (`5903aa3d`: the seed
    /// from the new digest, whatever the target), for byte comparisons.
    fn before_b31(old: &Request, kind: &str, parent_words: usize, attempts_remaining: u32, r05: &R05) -> Request {
        let target = plan::mirror_target_words_v2(parent_words, r05);
        let prompt = old.prompt.replacen(&format!(" in English, about {} words long.", old.target_words), &format!(" in English, about {target} words long."), 1);
        let rs = plan::request_sha(&old.generator, &old.endpoint, &digest_template(old, kind).unwrap(), &prompt, &old.decoding);
        Request {
            request_id: format!("req-{}", &sha(format!("{}:{MIRROR_TARGET_V2_ID}", old.request_id).as_bytes())[..24]),
            prompt,
            target_words: target,
            seed: plan::seed_of(&rs),
            request_sha256: rs,
            attempt_limit: Some(attempts_remaining),
            ..old.clone()
        }
    }

    fn without_seed(r: &Request) -> Value {
        let mut v = serde_json::to_value(r).unwrap();
        v.as_object_mut().unwrap().remove("seed").unwrap();
        v
    }

    #[test]
    fn a_new_mirror_request_keeps_the_digest_template_and_asks_for_v2_words() {
        let r05 = R05::parse("0.8").unwrap();
        // plan v1 and v3 hash the template id; plan v2 hashes `<template id>:<kind>` (B5b review mutation R11)
        for t in ["mirror_v1", "mirror_v1:mirror"] {
            let o = old(t, 121);
            let n = new_mirror_request(&o, "mirror", 121, 1, 1, &r05).unwrap();
            assert_eq!(n.target_words, 188);
            assert_eq!(n.prompt, o.prompt.replace(" about 121 words long.", " about 188 words long."));
            assert_eq!(n.request_sha256, plan::request_sha(&o.generator, &o.endpoint, t, &n.prompt, &o.decoding), "{t}");
            assert_eq!(n.seed, plan::seed_of(&n.request_sha256));
            assert_eq!(n.attempt_limit, Some(1));
            assert_eq!(n.request_id, format!("req-{}", &sha(b"req-old:mirror_target_words_v2")[..24]));
            assert_eq!((n.item_id.as_str(), n.template_id.as_str(), n.cohort.as_deref()), ("it", "mirror_v1", Some("c")));
        }
        // a parent longer than the floor keeps its length
        let n = new_mirror_request(&old("mirror_v1", 400), "mirror", 400, 0, 2, &r05).unwrap();
        assert_eq!((n.target_words, n.attempt_limit), (400, Some(2)));
        // no template string reproduces the recorded digest
        let mut o = old("mirror_v1", 121);
        o.request_sha256 = "0".repeat(64);
        assert!(new_mirror_request(&o, "mirror", 121, 1, 1, &r05).unwrap_err().to_string().contains("no template string"));
    }

    #[test]
    fn an_unchanged_target_mirror_continues_the_attempt_index_and_a_lifted_one_keeps_its_bytes() {
        // amendment 50 item 50.1 (W19-R3; B31)
        let r05 = R05::parse("0.8").unwrap();
        for t in ["mirror_v1", "mirror_v1:mirror"] {
            // R0's two-attempt row at or above the floor (188): attempt 0 failed at seed s; the one remaining attempt is
            // the item's attempt 1 at s + 1, with the old request's prompt, digest and target
            for pw in [188, 400] {
                let o = old(t, pw);
                let n = new_mirror_request(&o, "mirror", pw, 1, 1, &r05).unwrap();
                assert_eq!((n.prompt.as_str(), n.request_sha256.as_str(), n.target_words), (o.prompt.as_str(), o.request_sha256.as_str(), pw));
                assert_eq!(n.seed, o.seed.wrapping_add(1), "{t} {pw}");
                assert_eq!(n.seed, plan::attempt_seed(o.seed, 1));
                assert_ne!(n.seed, o.seed);
                // the request differs from the code before B31 only in its seed (id, digest, prompt and limit kept)
                let b = before_b31(&o, "mirror", pw, 1, &r05);
                assert_eq!(b.seed, o.seed, "the replay B31 fixes: the code before it sent the remaining attempt at s");
                assert_eq!(without_seed(&n), without_seed(&b));
                assert_eq!(n.attempt_limit, Some(1));
            }
            // a row with no attempt used (limit 2, remaining 2) has no earlier attempt: attempt 0 at s
            let o = old(t, 400);
            let n = new_mirror_request(&o, "mirror", 400, 0, 2, &r05).unwrap();
            assert_eq!((n.seed, n.attempt_limit), (o.seed, Some(2)));
            // the seed wraps in u32
            let mut o = old(t, 400);
            o.seed = u32::MAX;
            assert_eq!(new_mirror_request(&o, "mirror", 400, 1, 1, &r05).unwrap().seed, 0);
            // a carried limit (a request that already carries 3): the index continues past the used attempts
            let mut o = old(t, 400);
            o.attempt_limit = Some(3);
            let n = new_mirror_request(&o, "mirror", 400, 2, 1, &r05).unwrap();
            assert_eq!((n.seed, n.attempt_limit), (o.seed.wrapping_add(2), Some(1)));
            // a lifted mirror (below the floor): a new body, its digest's seed and attempts from 0, byte-identical to
            // the code before B31
            for pw in [121, 187] {
                let o = old(t, pw);
                let n = new_mirror_request(&o, "mirror", pw, 1, 1, &r05).unwrap();
                assert_eq!(n.target_words, 188);
                assert_ne!(n.request_sha256, o.request_sha256);
                assert_eq!(n.seed, plan::seed_of(&n.request_sha256));
                assert_eq!(serde_json::to_vec(&n).unwrap(), serde_json::to_vec(&before_b31(&o, "mirror", pw, 1, &r05)).unwrap(), "{t} {pw}");
            }
        }
    }

    #[test]
    fn a_new_mirror_request_is_refused_off_the_attempt_and_length_rules() {
        let r05 = R05::parse("0.8").unwrap();
        // the old target is not the parent's words (B5b review mutation R2): R0's mirrors were requested at the parent's length
        let e = new_mirror_request(&old("mirror_v1", 120), "mirror", 121, 1, 1, &r05).unwrap_err().to_string();
        assert!(e.contains("is not its parent's words"), "{e}");
        // no attempt left
        let e = new_mirror_request(&old("mirror_v1", 121), "mirror", 121, 2, 0, &r05).unwrap_err().to_string();
        assert!(e.contains("no attempt remains"), "{e}");
        // used and remaining attempts that do not make the old request's limit (B31: the seed offset is the used count)
        for (used, left) in [(0, 1), (2, 1), (1, 2), (u32::MAX, 1)] {
            let e = new_mirror_request(&old("mirror_v1", 400), "mirror", 400, used, left, &r05).unwrap_err().to_string();
            assert!(e.contains("do not make its limit 2"), "{used} {left}: {e}");
        }
        // a span request, a completion request, and a prompt that states its length twice or not at all
        let mut o = old("mirror_v1", 121);
        o.source_span = Some((0, 3));
        assert!(new_mirror_request(&o, "mirror", 121, 1, 1, &r05).unwrap_err().to_string().contains("not a mirror request"));
        let mut o = old("mirror_v1", 121);
        o.endpoint = "completion".into();
        assert!(new_mirror_request(&o, "mirror", 121, 1, 1, &r05).unwrap_err().to_string().contains("not a mirror request"));
        let mut o = old("mirror_v1", 121);
        o.prompt = format!("{} Again: in English, about 121 words long.", o.prompt);
        assert!(new_mirror_request(&o, "mirror", 121, 1, 1, &r05).unwrap_err().to_string().contains("exactly once"));
    }
}
