//! The Train-only planner for new mirrors (B5b part 2, item 1; BUILD-PLAN
//! 11.4; protocol A2 §6.3 and amendment 24 item 24.4's item 1).
//!
//! `mirror_target_words_v2` is §6.3's length for "every new or regenerated
//! mirror in every population". Part 1 regenerates R0's failed mirrors at it
//! (`mirror_regen`); this module plans **new** mirrors at it, for D5's
//! top-up mirrors (`topup`) and D6's Enron positives (`enron_plan`), where
//! `plan` and `plan-v2` need `--dev-select`, read every input they record
//! and request mirrors at the parent's length.
//!
//! **Inputs.** One Train input file, which `generate` and `finalize` later
//! read whole (`load_humans`), so it holds one split: every line's
//! `frozen_split` is read from its bytes before any parse (amendment 15 item
//! 15.4's reading, `renorm::line_split`), and a line naming another split
//! (`dev_select` and `test` among them) or none that can be read refuses the
//! plan by line number, unparsed (ruling R2's one-split rule for plan
//! files). There is no `--dev-select` and no input of another split is
//! parsed. Train rows take `plan::train_only_domain_of` (enron-v1's Train
//! half is a domain here and in this planner's `generate` and `finalize`
//! runs only). The CAL input is required: `--cal-from PLAN_DIR` names an R0
//! local plan (v1 or v2, or a one-split copy of one), whose recorded CAL
//! input record is copied unchanged as `generate`'s overlap extra, so the
//! plan keeps R0's screen (amendment 24 item 24.2; amendment 27); the
//! planner opens no CAL file and no other input of that plan
//! ([`OVERLAP_READING`]).
//!
//! **The rule** ([`TRAIN_MIRROR_RULE`]). One mirror per parent (R0's plan v1
//! gives each Train human one mirror):
//! * the request asks for `plan::mirror_target_words_v2` words, with r05 read
//!   from `VALUES-R2.json`'s S0-6 (`values::r05`; refused while S0-6 is
//!   absent) and `plan::mirror_prompt_words`: R0's mirror template
//!   (`a_comp_mirror_v1`), its key terms from the plan's own Train humans'
//!   per-domain IDF;
//! * generators are the seven Train instruct families (`plan::FAMILY_ORDER`;
//!   protocol A2 §4.4 D5 and D6: "7 Train instruct families"), never a
//!   hosted model. Within each domain the parents are taken in ascending V21
//!   key `SHA-256("slop_ninja_a_comp_r2_plan_v1:train_mirror_gen:" + id)` and
//!   each takes the next family, in `FAMILY_ORDER` cycle, that has quota left;
//!   a domain's quota is split equally over the families (the first
//!   `n mod 7` families get one more), after any per-family minimum (D5's
//!   rule-3 cells, from the top-up plan);
//! * decoding is R0's Train profile `plan::train_decoding` of the rank of
//!   V21 key `train_mirror_decoding` within (train, domain);
//! * a request carries no `attempt_limit`: R0's two attempts per request
//!   (`generation::ATTEMPTS`, the inherited per-item limit).
//!
//! **The plan** holds one split (`train`), so `generate` and `finalize`
//! accept it (ruling R2). Its items are one batch under `generate --profile
//! cc_cohort_profile_v2 --profile-population train2 --splits train` on its
//! own runs directory: the prior is the plan's parents (item 10.8; the
//! batch rule [`BATCH_READING`], fixed by amendment 27).
use crate::plan::{self, Decoding, Human, Item, MIRROR_TEMPLATE_ID, Plan, R05, Request};
use crate::split_copy::{new_dir, write_new};
use crate::{span_ops, text};
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::Path;

/// `plan.json`'s schema of a Train-only mirror plan.
pub const TRAIN_MIRROR_SCHEMA: &str = "slop_ninja_a_comp_positive_plan_train_mirror_v1";
/// The cohort of a plan of `train-mirror-plan` without a top-up selection.
pub const COHORT_TRAIN_MIRROR: &str = "train2_mirror";
/// D5's top-up mirrors (`topup`).
pub const COHORT_TOPUP_MIRROR: &str = "train2_topup_mirror";
/// D6's Enron Train-half mirrors (`enron_plan`).
pub const COHORT_ENRON_MIRROR: &str = "train2_enron_mirror";
/// The cohorts this planner writes.
pub const COHORTS: [&str; 3] = [COHORT_TRAIN_MIRROR, COHORT_TOPUP_MIRROR, COHORT_ENRON_MIRROR];
/// V21's x of the family assignment order.
pub const KEY_GEN: &str = "train_mirror_gen";
/// V21's x of the decoding rank.
pub const KEY_DECODING: &str = "train_mirror_decoding";
/// The planner's rule, as records state it.
pub const TRAIN_MIRROR_RULE: &str = "train_mirror_plan_v1 (B5b part 2 item 1; protocol A2 section 6.3, amendment 24 item 24.4, amendment 27): one mirror per parent of one Train input (frozen_split train only: each line's split read from its bytes, and a line of another split or of none refuses the plan unparsed; no dev_select input), requested at mirror_target_words_v2 = max(parent words, ceil(150 / r05)) with r05 read from VALUES-R2.json's S0-6 (refused while absent), R0's mirror template a_comp_mirror_v1 with 8 key terms from the plan's Train humans' per-domain IDF; generators the seven Train instruct families (FAMILY_ORDER): within a domain parents in ascending V21 key SHA-256('slop_ninja_a_comp_r2_plan_v1:train_mirror_gen:' + id) (no A6 key), ties by id, each taking the next family in FAMILY_ORDER cycle with quota left, the domain's quota split equally after any per-family minimum (the first n mod 7 families one more); decoding plan::train_decoding(rank of V21 key train_mirror_decoding within (train, domain); no A6 key); no attempt_limit (R0's two attempts per request); the CAL overlap extra copied from an R0 local plan (required)";
/// Which batch, binding and prior judge the plan's requests (BUILD-PLAN
/// 11.4's open item (c); fixed by amendment 27).
pub const BATCH_READING: &str = "a Train-only mirror plan is one D-step's plan and one batch (amendment 10 item 10.8: one prior per batch, written once over the plan's parents before its pilot; amendment 27): generate --profile cc_cohort_profile_v2 --profile-population train2 --splits train --plan <plan> on a runs directory of its own, so its binding (form-profile-split-train.json) and prior are the plan's parents only; the pilot (pilot --plan <plan>) and its bulk are request orders of that batch";
/// The overlap rule (BUILD-PLAN 11.4's open item (b); fixed by amendment
/// 27).
pub const OVERLAP_READING: &str = "every Train-only mirror plan keeps R0's screen (amendment 24 item 24.2: the protected index, CAL as generate's overlap extra, and the accepted generated texts; amendment 27): --cal-from PLAN_DIR (required) names an R0 local plan v1 or v2, or a one-split copy of one, whose plan.json records a cal input; that input record (path and digest) is copied unchanged into the new plan's inputs.cal, and the source plan.json's path and digest are recorded; the planner opens no CAL file and no other input of the source plan; generate reads the CAL rows for overlap keys only, as for D4's copies; a consumed-Test input is never recorded";

/// The plan schemas `--cal-from` takes: R0's local plans v1 and v2 (a
/// one-split copy keeps its source's schema).
pub const CAL_FROM_SCHEMAS: [&str; 2] = [plan::PLAN_SCHEMA, crate::diverse::PLAN_V2_SCHEMA];

/// The CAL input record of the R0 local plan `src` ([`OVERLAP_READING`]):
/// `(inputs.cal record, the source record)`. Opens `src/plan.json` only.
pub fn cal_from(src: &Path) -> Result<(Value, Value)> {
    let pp = src.join("plan.json");
    let b = fs::read(&pp).with_context(|| pp.display().to_string())?;
    let m: Value = serde_json::from_slice(&b).with_context(|| pp.display().to_string())?;
    ensure!(CAL_FROM_SCHEMAS.iter().any(|x| m["schema"] == *x), "--cal-from {}: not an R0 local plan v1 or v2 (or a one-split copy of one)", src.display());
    let c = &m["inputs"][crate::split_copy::OVERLAP_EXTRA];
    ensure!(c["path"].is_string() && c["sha256"].as_str().is_some_and(|h| h.len() == 64 && h.bytes().all(|x| x.is_ascii_hexdigit())),
        "--cal-from {}: its plan.json records no CAL input (path and sha256)", src.display());
    let rec = json!({"path": c["path"], "sha256": c["sha256"]});
    Ok((rec, json!({"plan_json": fs::canonicalize(&pp)?.display().to_string(), "sha256": text::sha_hex(&b), "schema": m["schema"], "split_copy": m.get("split_copy").is_some()})))
}

/// A Train input file: its parsed Train humans and its digest.
pub struct TrainInput {
    pub humans: Vec<Human>,
    pub sha256: String,
    pub lines: usize,
}

/// Reads the Train humans of `path` (see the module notes): each line's
/// `frozen_split` read from its bytes before any parse; `train` lines are
/// parsed with `plan::human_from`, and a line of another split, or whose
/// split cannot be read, refuses the file by line number unparsed.
pub fn read_train(path: &Path) -> Result<TrainInput> {
    let f = fs::File::open(path).with_context(|| path.display().to_string())?;
    let mut rd = BufReader::with_capacity(1 << 20, f);
    let mut hasher = <sha2::Sha256 as sha2::Digest>::new();
    let mut humans = Vec::new();
    let mut buf = Vec::new();
    let mut n = 0usize;
    loop {
        buf.clear();
        if rd.read_until(b'\n', &mut buf)? == 0 {
            break;
        }
        sha2::Digest::update(&mut hasher, &buf);
        n += 1;
        let l = buf.strip_suffix(b"\n").unwrap_or(&buf);
        if l.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        match crate::renorm::line_split(l, "frozen_split") {
            crate::renorm::LineSplit::One(s) if s == "train" => {
                let row: Value = serde_json::from_slice(l).with_context(|| format!("{} line {n}", path.display()))?;
                humans.push(plan::human_from_in(&row, "train", true).with_context(|| format!("{} line {n}", path.display()))?);
            }
            crate::renorm::LineSplit::One(s) => bail!(
                "{} line {n}: names {} ({s}): a Train-only plan's input holds Train lines only, since generate and finalize read it whole (ruling R2); refused from the line's split bytes, before any parse",
                path.display(),
                crate::split_copy::split_class(&s)
            ),
            _ => bail!("{} line {n}: no frozen_split that can be read from the line's bytes; refused before any parse", path.display()),
        }
    }
    let mut seen = std::collections::HashSet::new();
    for h in &humans {
        ensure!(seen.insert(h.id.as_str()), "{}: duplicate human id {}", path.display(), h.id);
    }
    Ok(TrainInput { humans, sha256: hex::encode(sha2::Digest::finalize(hasher)), lines: n })
}

/// Per-family quotas of `n` mirrors in one domain: each family's minimum
/// (`mins`, keyed by family; none outside `FAMILY_ORDER`), then the rest
/// split equally in `FAMILY_ORDER` (the first `rest mod 7` one more). When
/// `n` is below the minimums' sum, the minimums are met in `FAMILY_ORDER`
/// until `n` runs out (the top-up records the shortfall).
pub fn family_quotas(n: usize, mins: &BTreeMap<String, usize>) -> Result<Vec<(&'static str, usize)>> {
    for f in mins.keys() {
        ensure!(plan::FAMILY_ORDER.contains(&f.as_str()), "{f} is not a Train instruct family (FAMILY_ORDER)");
    }
    let mut left = n;
    let mut q: Vec<(&'static str, usize)> = plan::FAMILY_ORDER
        .iter()
        .map(|f| {
            let m = mins.get(*f).copied().unwrap_or(0).min(left);
            left -= m;
            (*f, m)
        })
        .collect();
    let k = q.len();
    let (each, extra) = (left / k, left % k);
    for (i, (_, c)) in q.iter_mut().enumerate() {
        *c += each + usize::from(i < extra);
    }
    debug_assert_eq!(q.iter().map(|x| x.1).sum::<usize>(), n);
    Ok(q)
}

/// The family of each parent of one domain: parents in ascending
/// [`KEY_GEN`] key (ties by id), each taking the next family in
/// `FAMILY_ORDER` cycle that has quota left. `quotas` must sum to the
/// parents' count.
pub fn assign<'a>(parents: &[&'a Human], quotas: &[(&'static str, usize)]) -> Result<Vec<(&'a Human, &'static str)>> {
    ensure!(quotas.iter().map(|x| x.1).sum::<usize>() == parents.len(), "family quotas ({}) differ from the parents ({})",
        quotas.iter().map(|x| x.1).sum::<usize>(), parents.len());
    let mut keyed: Vec<(String, &&Human)> = parents.iter().map(|h| (span_ops::r2_key(KEY_GEN, &h.id), h)).collect();
    keyed.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.id.cmp(&b.1.id)));
    let mut left: Vec<usize> = quotas.iter().map(|x| x.1).collect();
    let mut at = 0usize;
    let mut out = Vec::new();
    for (_, h) in keyed {
        let mut k = 0;
        while left[(at + k) % left.len()] == 0 {
            k += 1;
        }
        let i = (at + k) % left.len();
        left[i] -= 1;
        out.push((*h, quotas[i].0));
        at = i + 1;
    }
    Ok(out)
}

/// The item id of a new mirror of `parent` in `cohort`.
pub fn item_id(cohort: &str, parent: &str) -> String {
    format!("pos-{}", &text::sha_hex(format!("{}{cohort}:train:{parent}", span_ops::R2_PLAN_KEY_PREFIX).as_bytes())[..24])
}

/// Items and requests of the new mirrors of `assigned` in `cohort` at
/// `mirror_target_words_v2` (r05 `r05`), key terms from `idf`.
pub fn build(assigned: &[(&Human, &'static str)], cohort: &str, r05: &R05, idf: &HashMap<&str, text::Idf>) -> Result<Plan> {
    ensure!(COHORTS.contains(&cohort), "cohort {cohort} is not a Train-only mirror cohort ({COHORTS:?})");
    let stop = text::stopwords();
    let hs: Vec<&Human> = assigned.iter().map(|(h, _)| *h).collect();
    let dec_rank = span_ops::r2_ranks(&hs, KEY_DECODING);
    let gen_rank = span_ops::r2_ranks(&hs, KEY_GEN);
    let mut items = Vec::new();
    let mut requests = Vec::new();
    for (h, g) in assigned {
        ensure!(h.split == "train", "parent {}: not a Train human", h.id);
        ensure!(plan::FAMILY_ORDER.contains(g), "{g} is not a Train instruct family");
        let dec: Decoding = plan::train_decoding(dec_rank[&h.id]);
        let iid = item_id(cohort, &h.id);
        let target = plan::mirror_target_words_v2(h.word_count, r05);
        let terms = text::key_terms(&h.text, idf.get(h.domain).with_context(|| format!("no IDF for domain {}", h.domain))?, &stop, 8);
        let prompt = plan::mirror_prompt_words(h, &terms, target);
        let rs = plan::request_sha(g, "chat", MIRROR_TEMPLATE_ID, &prompt, &dec);
        let rid = format!("req-{}", &text::sha_hex(format!("{iid}:0").as_bytes())[..24]);
        requests.push(Request {
            request_id: rid.clone(),
            item_id: iid.clone(),
            parent_id: h.id.clone(),
            split: "train".into(),
            generator: g.to_string(),
            endpoint: "chat".into(),
            template_id: MIRROR_TEMPLATE_ID.into(),
            prompt,
            span_index: 0,
            source_span: None,
            target_words: target,
            cut_budget: None,
            decoding: dec.clone(),
            seed: plan::seed_of(&rs),
            request_sha256: rs,
            op: None,
            cohort: Some(cohort.into()),
            attempt_limit: None,
        });
        items.push(Item {
            item_id: iid,
            split: "train".into(),
            domain: h.domain.into(),
            parent_id: h.id.clone(),
            parent_text_sha256: h.text_sha256.clone(),
            source_group: h.source_group.clone(),
            cohort: cohort.into(),
            kind: "mirror".into(),
            bin: None,
            target_bp: None,
            anchor: None,
            multi_span: false,
            generator: g.to_string(),
            decoding: dec,
            template_id: MIRROR_TEMPLATE_ID.into(),
            source_spans: vec![],
            requested_fraction: None,
            requests: vec![rid],
            notes: vec![plan::MIRROR_TARGET_V2_ID.into()],
            ranks: [(KEY_GEN.to_string(), gen_rank[&h.id]), (KEY_DECODING.to_string(), dec_rank[&h.id])].into(),
            operation: None,
            style: None,
        });
    }
    let mut seen = std::collections::HashSet::new();
    for it in &items {
        ensure!(seen.insert(it.item_id.as_str()), "parent of item {} planned twice", it.item_id);
    }
    Ok(Plan { items, requests, failures: vec![] })
}

/// The per-domain IDF of `humans` (the plan's Train humans).
pub fn idf_of(humans: &[Human]) -> HashMap<&'static str, text::Idf> {
    let stop = text::stopwords();
    let mut idf: HashMap<&'static str, text::Idf> = HashMap::new();
    for h in humans {
        idf.entry(h.domain).or_default().add(&h.text, &stop);
    }
    idf
}

/// Quota groups: each parent's group by id, and each group's family quotas.
pub type Groups = (HashMap<String, String>, BTreeMap<String, Vec<(&'static str, usize)>>);

/// What a plan of this planner is made of, besides the Train input.
pub struct Spec<'a> {
    pub cohort: &'a str,
    /// The quota groups (D5: S0-8's domains, from the top-up plan, whose
    /// quotas start at the rule-3 minimums); `None`: one group per planner
    /// domain with equal quotas.
    pub groups: Option<Groups>,
    /// The step's own record (the top-up selection or the Enron binding).
    pub source: Value,
    /// The Train input's digest the step checked (D5: the top-up plan's
    /// parents file, read by `topup::groups_of`); the plan run re-reads the
    /// file and refuses another digest.
    pub train_sha256: Option<String>,
}

/// Plans one mirror per Train human of `train` under `spec` and writes the
/// plan to the new directory `out` (see the module notes). `cal_src` is the
/// R0 local plan whose CAL input record the plan keeps ([`cal_from`]).
/// Returns the plan's `train_mirror` record.
pub fn run(train: &Path, cal_src: &Path, values: &crate::values::Values, spec: &Spec, out: &Path) -> Result<Value> {
    ensure!(!out.exists(), "{} exists: the plan is a new directory", out.display());
    let (r05, values_rec) = crate::values::r05(values)?;
    let (cal_rec, cal_src_rec) = cal_from(cal_src)?;
    let ti = read_train(train)?;
    if let Some(want) = &spec.train_sha256 {
        ensure!(ti.sha256 == *want, "{} changed since the step checked it", train.display());
    }
    ensure!(!ti.humans.is_empty(), "{}: no Train human", train.display());
    let idf = idf_of(&ti.humans);
    let mut by_group: BTreeMap<String, Vec<&Human>> = BTreeMap::new();
    for h in &ti.humans {
        let g = match &spec.groups {
            Some((of, _)) => of.get(&h.id).with_context(|| format!("parent {} is in no quota group of the top-up plan", h.id))?.clone(),
            None => h.domain.to_string(),
        };
        by_group.entry(g).or_default().push(h);
    }
    let mut assigned: Vec<(&Human, &'static str)> = Vec::new();
    let mut quota_rec = serde_json::Map::new();
    for (g, hs) in &by_group {
        let q = match &spec.groups {
            Some((_, m)) => m.get(g).cloned().with_context(|| format!("the top-up plan gives no family quotas for group {g}"))?,
            None => family_quotas(hs.len(), &BTreeMap::new())?,
        };
        quota_rec.insert(g.clone(), json!(q.iter().map(|(f, n)| (f.to_string(), json!(n))).collect::<serde_json::Map<_, _>>()));
        assigned.extend(assign(hs, &q)?);
    }
    if let Some((_, m)) = &spec.groups {
        for (g, q) in m {
            ensure!(by_group.contains_key(g) || q.iter().all(|x| x.1 == 0), "the top-up plan plans mirrors in group {g}, which the Train input lacks");
        }
    }
    let p = build(&assigned, spec.cohort, &r05, &idf)?;
    // the plan directory: one split, train
    new_dir(out)?;
    new_dir(&out.join("requests"))?;
    let jl = |v: &[Value]| -> Result<Vec<u8>> {
        let mut b = Vec::new();
        for x in v {
            serde_json::to_writer(&mut b, x)?;
            b.push(b'\n');
        }
        Ok(b)
    };
    let items_v: Vec<Value> = p.items.iter().map(serde_json::to_value).collect::<Result<_, _>>()?;
    let items_sha = write_new(&out.join("items.jsonl"), &jl(&items_v)?)?;
    let fails_sha = write_new(&out.join("plan-failures.jsonl"), b"")?;
    let mut by_gen: BTreeMap<&str, Vec<Value>> = BTreeMap::new();
    for r in &p.requests {
        by_gen.entry(r.generator.as_str()).or_default().push(serde_json::to_value(r)?);
    }
    let mut req_files = serde_json::Map::new();
    for (g, v) in &by_gen {
        let h = write_new(&out.join("requests").join(format!("{g}.jsonl")), &jl(v)?)?;
        req_files.insert(g.to_string(), json!({"requests": v.len(), "sha256": h}));
    }
    let mut humans_by: BTreeMap<&str, usize> = BTreeMap::new();
    let mut window: BTreeMap<&str, BTreeMap<&str, usize>> = BTreeMap::new();
    let (lo, hi) = grammar_a_form::window::LENGTH_WINDOW;
    for h in &ti.humans {
        *humans_by.entry(h.domain).or_default() += 1;
        let w = if h.word_count < lo { "below" } else if h.word_count > hi { "above" } else { "inside" };
        *window.entry(h.domain).or_default().entry(w).or_default() += 1;
    }
    let mut cells: BTreeMap<String, usize> = BTreeMap::new();
    let mut targets: BTreeMap<String, usize> = BTreeMap::new();
    for ((it, r), (h, _)) in p.items.iter().zip(&p.requests).zip(&assigned) {
        *cells.entry(format!("{}|{}", it.domain, it.generator)).or_default() += 1;
        let k = if r.target_words > h.word_count { "raised_to_ceil_150_over_r05" } else { "parent_words" };
        *targets.entry(k.into()).or_default() += 1;
    }
    let mut inputs = serde_json::Map::new();
    inputs.insert("train".into(), json!({"path": fs::canonicalize(train)?.display().to_string(), "sha256": ti.sha256}));
    inputs.insert(crate::split_copy::OVERLAP_EXTRA.into(), cal_rec);
    let idf_meta: BTreeMap<&str, Value> = idf.iter().map(|(d, t)| (*d, json!({"train_documents": t.n_docs, "types": t.df.len(), "sha256": t.digest()}))).collect();
    let rec = json!({"schema": TRAIN_MIRROR_SCHEMA, "rule": TRAIN_MIRROR_RULE, "cohort": spec.cohort, "batch": BATCH_READING, "overlap": OVERLAP_READING, "cal_from": cal_src_rec,
        "s0_8": spec.source.get("s0_8").cloned().unwrap_or(Value::Null),
        "values": values_rec, "mirror_target": r05.record(), "quota_groups": if spec.groups.is_some() { "the top-up plan's S0-8 domains" } else { "planner domains" }, "family_quotas": quota_rec, "cells_domain_generator": cells, "requested_words": targets,
        "parents_by_length_window": {"window": grammar_a_form::window::LENGTH_WINDOW_ID, "domains": window},
        "train_input": {"lines": ti.lines, "one_split": "train"}, "source": spec.source});
    let meta = json!({"schema": TRAIN_MIRROR_SCHEMA, "code": crate::CODE_ID, "inputs": inputs, "planned_splits": ["train"],
        "producer_sha256": {"train_mirror.rs": text::sha_hex(include_bytes!("train_mirror.rs")), "plan.rs": text::sha_hex(include_bytes!("plan.rs"))},
        "key_prefix": span_ops::R2_PLAN_KEY_PREFIX, "terms": text::TERMS_ID, "idf": idf_meta, "humans": {"train": humans_by},
        "items": p.items.len(), "items_sha256": items_sha, "requests": p.requests.len(), "request_files": req_files,
        "plan_failures": 0, "plan_failures_sha256": fails_sha, "train_mirror": rec});
    let mut b = serde_json::to_vec_pretty(&meta)?;
    b.push(b'\n');
    let h = write_new(&out.join("plan.json"), &b)?;
    let mut r = meta["train_mirror"].clone();
    r["plan_sha256"] = json!(h);
    r["items"] = json!(p.items.len());
    Ok(r)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn human(id: &str, domain: &'static str, words: usize) -> Human {
        let t = (0..words).map(|i| format!("w{}", i % 13)).collect::<Vec<_>>().join(" ");
        Human {
            split: "train".into(),
            domain,
            id: id.into(),
            text_sha256: text::sha_hex(t.as_bytes()),
            word_count: text::words(&t),
            text: t,
            title: None,
            source_group: format!("g-{id}"),
            genre: "x".into(),
        }
    }

    #[test]
    fn family_quotas_split_equally_after_the_minimums() {
        let q = family_quotas(16, &BTreeMap::new()).unwrap();
        assert_eq!(q.iter().map(|x| x.1).collect::<Vec<_>>(), [3, 3, 2, 2, 2, 2, 2]);
        assert_eq!(q.iter().map(|x| x.0).collect::<Vec<_>>(), plan::FAMILY_ORDER);
        let mins: BTreeMap<String, usize> = [("ernie_instruct".to_string(), 5), ("phi4mini".to_string(), 2)].into();
        let q: BTreeMap<&str, usize> = family_quotas(14, &mins).unwrap().into_iter().collect();
        // 7 to the minimums, then 7 split one each
        assert_eq!((q["ernie_instruct"], q["phi4mini"], q["qwen36"]), (6, 3, 1));
        assert_eq!(q.values().sum::<usize>(), 14);
        // below the minimums' sum: met in FAMILY_ORDER until n runs out
        let q: BTreeMap<&str, usize> = family_quotas(4, &mins).unwrap().into_iter().collect();
        assert_eq!((q["phi4mini"], q["ernie_instruct"], q["qwen36"]), (2, 2, 0));
        assert!(family_quotas(3, &[("gptoss20b".to_string(), 1)].into()).is_err());
    }

    #[test]
    fn assignment_cycles_families_with_quota_in_key_order() {
        let hs: Vec<Human> = (0..10).map(|i| human(&format!("h{i}"), "wikipedia", 30)).collect();
        let refs: Vec<&Human> = hs.iter().collect();
        let q = family_quotas(10, &BTreeMap::new()).unwrap();
        let a = assign(&refs, &q).unwrap();
        // independent of `assign`: key order spelled out, then FAMILY_ORDER cycling
        let mut keyed: Vec<(String, &str)> = hs.iter().map(|h| (text::sha_hex(format!("slop_ninja_a_comp_r2_plan_v1:train_mirror_gen:{}", h.id).as_bytes()), h.id.as_str())).collect();
        keyed.sort();
        let want: Vec<(&str, &str)> = keyed.iter().enumerate().map(|(i, (_, id))| (*id, plan::FAMILY_ORDER[i % 7])).collect();
        let got: Vec<(&str, &str)> = a.iter().map(|(h, g)| (h.id.as_str(), *g)).collect();
        assert_eq!(got, want);
        // quotas with one family exhausted: the cycle skips it
        let q = vec![("qwen36", 0), ("phi4mini", 3), ("mistral7b", 0), ("gemma3_4b", 0), ("granite_instruct", 0), ("smollm3_instruct", 0), ("ernie_instruct", 7)];
        let a = assign(&refs, &q).unwrap();
        let mut c: BTreeMap<&str, usize> = BTreeMap::new();
        for (_, g) in &a {
            *c.entry(g).or_default() += 1;
        }
        assert_eq!((c["phi4mini"], c["ernie_instruct"]), (3, 7));
        assert!(assign(&refs, &family_quotas(9, &BTreeMap::new()).unwrap()).is_err());
    }

    #[test]
    fn new_mirrors_ask_for_mirror_target_words_v2() {
        let hs = [human("a", "wikipedia", 120), human("b", "wikipedia", 300), human("c", "legal_opinions", 188)];
        let refs: Vec<&Human> = hs.iter().collect();
        let a: Vec<(&Human, &'static str)> = refs.iter().map(|h| (*h, "qwen36")).collect();
        let r05 = R05::parse("13/16").unwrap(); // ceil(150 * 16 / 13) = 185
        let p = build(&a, COHORT_TOPUP_MIRROR, &r05, &idf_of(&hs)).unwrap();
        let t: Vec<usize> = p.requests.iter().map(|r| r.target_words).collect();
        assert_eq!(t, [185, 300, 188]);
        for (r, h) in p.requests.iter().zip(&hs) {
            assert!(r.prompt.contains(&format!(" in English, about {} words long.", r.target_words)), "{}", r.prompt);
            assert_eq!(r.request_sha256, plan::request_sha(&r.generator, "chat", MIRROR_TEMPLATE_ID, &r.prompt, &r.decoding));
            assert_eq!((r.split.as_str(), r.attempt_limit, r.parent_id.as_str()), ("train", None, h.id.as_str()));
            assert_eq!(r.attempts(), crate::generation::ATTEMPTS);
        }
        assert!(p.items.iter().all(|i| i.kind == "mirror" && i.cohort == COHORT_TOPUP_MIRROR && i.split == "train"));
        assert!(build(&a, "primary_mirror", &r05, &idf_of(&hs)).is_err());
        // a prompt at the parent's length is plan v1's prompt, byte for byte
        let h = &hs[0];
        assert_eq!(plan::mirror_prompt_words(h, &["x".into()], h.word_count), plan::mirror_prompt(h, &["x".into()]));
    }

    #[test]
    fn the_plan_run_refuses_a_train_input_other_than_the_one_its_step_checked() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        let t = (0..30).map(|i| format!("w{}", i % 13)).collect::<Vec<_>>().join(" ");
        let row = json!({"id": "h1", "frozen_split": "train", "origin": "human_only", "text": t, "text_sha256": text::sha_hex(t.as_bytes()),
            "word_count": text::words(&t), "collection": "wikipedia_enwiki_historical", "provenance": {}, "source_group": "g", "domain": "web"});
        let tr = d.join("train.jsonl");
        fs::write(&tr, format!("{row}\n")).unwrap();
        let vp = d.join("values.json");
        fs::write(&vp, serde_json::to_vec(&json!({"schema": crate::values::VALUES_SCHEMA, "values": {"S0-6": {"id": "S0-6", "name": "r05", "value": "0.8"}}})).unwrap()).unwrap();
        let src = d.join("r0");
        fs::create_dir_all(&src).unwrap();
        fs::write(src.join("plan.json"), serde_json::to_vec(&json!({"schema": plan::PLAN_SCHEMA, "inputs": {"cal": {"path": "/x/cal.jsonl", "sha256": "ab".repeat(32)}}})).unwrap()).unwrap();
        let values = crate::values::Values::open(&vp).unwrap();
        let spec = |sha: Option<String>| Spec { cohort: COHORT_TRAIN_MIRROR, groups: None, source: json!({"step": null}), train_sha256: sha };
        let e = run(&tr, &src, &values, &spec(Some("0".repeat(64))), &d.join("p0")).err().unwrap().to_string();
        assert!(e.contains("changed since the step checked it"), "{e}");
        assert!(!d.join("p0").exists());
        let r = run(&tr, &src, &values, &spec(Some(text::sha_hex(&fs::read(&tr).unwrap()))), &d.join("p1")).unwrap();
        assert_eq!(r["items"], 1);
    }
}
