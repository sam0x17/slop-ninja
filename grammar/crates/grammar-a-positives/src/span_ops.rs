//! Plan v3: exact-span supervision cohorts.
//!
//! Two source-conditioned generation operations that give the share output
//! measured fractional supervision with exact per-word labels:
//!
//! - `prefix_continuation`: the human keeps a whole-sentence prefix holding
//!   30-70% of the passage's words (target drawn per item); an instruct
//!   model writes the rest. Labels are human then model.
//! - `middle_rewrite`: one interior whole-sentence run (15-50% of the words,
//!   with at least 10% of the words kept on each side) is replaced by a model
//!   passage written between the kept human context. Two modes, keyed per
//!   item: `middle_rewrite` shows the removed run and asks for a rewrite in
//!   context; `middle_infill` hides it and gives only its key terms. Labels
//!   are human, model, human.
//!
//! Attribution: the idea of human-prefix continuations and of regenerating an
//! interior span between kept human context, as a source of exact word
//! labels, comes from the MIT-licensed It's AI validator
//! (<https://github.com/It-s-AI/llm-detection>, Copyright 2024 It's AI). The
//! span selection, prompts, acceptance screens, keys and records below are
//! our own and run inside this planner; nothing from that repository is
//! copied.
//!
//! The plan is additive: it holds only the two new cohorts, keyed under its
//! own prefix, so its item and request ids never collide with plan v1/v2 and
//! existing runs stay valid. Outputs are source-conditioned rewrites and are
//! kept separate from writing generated from scratch (their own cohorts and
//! populations). No detector score is used anywhere.
//!
//! Base-model mode ([`build_v3_base`], round-2 build B6) plans the Train
//! base-model cohort instead: one item per Train human on the Train base
//! families, made by raw completion on `/completion` under the Tier C
//! robustness rules (the continuation and splice spans, verbatim prefix,
//! cut budget and decoding of `plan::build_test`'s base slice), continuation
//! and splice 1:1 by key, with exact spans. It plans no instruct cohort.
use crate::diverse;
use crate::plan::{self, Human, Item, Plan, Request};
use crate::text::{self, Idf};
use anyhow::{Result, bail, ensure};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet, HashMap};

pub const V3_KEY_PREFIX: &str = "slopninja-a-comp-plan-v3:";
pub const PLAN_V3_SCHEMA: &str = "slop_ninja_a_comp_positive_plan_v3";
pub const PREFIX_TEMPLATE_ID: &str = "a_comp_v3_prefix_continuation";
pub const MIDDLE_TEMPLATE_ID: &str = "a_comp_v3_middle_rewrite";
pub const COHORT_PREFIX: &str = "prefix_continuation";
pub const COHORT_MIDDLE: &str = "middle_rewrite";
/// Base-model mode's Train cohort (continuations and splices).
pub const COHORT_BASE: &str = "train_base_v3";
/// Plan v3 cohorts; finalize writes exact per-word label runs for each.
pub const COHORTS: [&str; 3] = [COHORT_PREFIX, COHORT_MIDDLE, COHORT_BASE];
/// Request operations (`Request::op`, `Item::operation`).
pub const OP_PREFIX: &str = "prefix_continue";
pub const OP_MIDDLE_REWRITE: &str = "middle_rewrite";
pub const OP_MIDDLE_INFILL: &str = "middle_infill";
pub const OPS: [&str; 3] = [OP_PREFIX, OP_MIDDLE_REWRITE, OP_MIDDLE_INFILL];

/// Kept human prefix, in basis points of the passage's words (inclusive).
pub const PREFIX_MIN_BP: u32 = 3000;
pub const PREFIX_MAX_BP: u32 = 7000;
/// Replaced interior run, basis points of words (target range).
pub const MIDDLE_MIN_BP: u32 = 1500;
pub const MIDDLE_MAX_BP: u32 = 5000;
/// Kept human context on each side of an interior run (minimum, inclusive).
pub const MIDDLE_SIDE_MIN_BP: u32 = 1000;
/// Interior-run midpoint target, basis points of the passage's words.
pub const MIDDLE_CENTER_MIN_BP: u32 = 3000;
pub const MIDDLE_CENTER_MAX_BP: u32 = 7000;
/// Key terms given to an infill request.
pub const INFILL_TERMS: usize = 6;
/// Copy-screen window (words) against kept and removed human text.
pub const COPY_WINDOW: usize = 12;
pub const RULES_ID: &str = "a_comp_plan_v3_span_ops_v1";
pub const BASE_RULES_ID: &str = "a_comp_plan_v3_base_mode_v1";
/// Round-2 plan keys (protocol A2 value V21, unkeyed):
/// `SHA-256("slop_ninja_a_comp_r2_plan_v1:" + x + ":" + d.id)`.
pub const R2_PLAN_KEY_PREFIX: &str = "slop_ninja_a_comp_r2_plan_v1:";
/// The V21 key name `x` of base-model mode's rank.
pub const BASE_KEY: &str = "base_cohort";

pub fn key(x: &str, id: &str) -> String {
    text::sha_hex(format!("{V3_KEY_PREFIX}{x}:{id}").as_bytes())
}

/// Uniform [0, 1] from the i-th 32-bit word of a hex digest.
fn unit(h: &str, i: usize) -> f64 {
    u32::from_str_radix(&h[8 * i..8 * i + 8], 16).unwrap() as f64 / u32::MAX as f64
}

/// Keyed basis-point draw in [lo, hi].
pub fn draw_bp(x: &str, item_id: &str, lo: u32, hi: u32) -> u32 {
    let h = key(x, item_id);
    lo + (unit(&h, 0) * (hi - lo) as f64).round() as u32
}

/// Ranks of k_x(d) within (split, domain), as in plans v1/v2.
pub fn ranks(humans: &[&Human], x: &str) -> HashMap<String, usize> {
    let mut by: BTreeMap<(&str, &str), Vec<(String, &str)>> = BTreeMap::new();
    for h in humans {
        by.entry((h.split.as_str(), h.domain)).or_default().push((key(x, &h.id), h.id.as_str()));
    }
    let mut out = HashMap::new();
    for (_, mut v) in by {
        v.sort();
        for (r, (_, id)) in v.into_iter().enumerate() {
            out.insert(id.to_owned(), r);
        }
    }
    out
}

/// Prefix cut: the whole-sentence boundary `j` (prefix = sentences [0, j))
/// whose kept word fraction is closest to `target_bp`, restricted to
/// [PREFIX_MIN_BP, PREFIX_MAX_BP]; ties go to the shorter prefix. None when no
/// boundary lands in range (for example a one- or two-sentence passage with
/// lopsided sentences).
pub fn prefix_cut(cum: &[usize], target_bp: u32) -> Option<usize> {
    let n = cum.len() - 1;
    let total = cum[n] as i64;
    if n < 2 || total == 0 {
        return None;
    }
    (1..n)
        .filter(|&j| {
            let k = cum[j] as i64 * 10000;
            k >= PREFIX_MIN_BP as i64 * total && k <= PREFIX_MAX_BP as i64 * total
        })
        .min_by_key(|&j| ((cum[j] as i64 * 10000 - target_bp as i64 * total).abs(), j))
}

/// Interior run [i, j) of whole sentences: at least one sentence and
/// MIDDLE_SIDE_MIN_BP of the words kept before and after; the run's word
/// fraction closest to `target_bp`, then its midpoint closest to
/// `center_bp`, then the earlier run.
pub fn middle_run(cum: &[usize], target_bp: u32, center_bp: u32) -> Option<(usize, usize)> {
    let n = cum.len() - 1;
    let total = cum[n] as i64;
    if n < 3 || total == 0 {
        return None;
    }
    let side = MIDDLE_SIDE_MIN_BP as i64 * total;
    // ((size gap, midpoint gap, start), run)
    type Ranked = ((i64, i64, usize), (usize, usize));
    let mut best: Option<Ranked> = None;
    for i in 1..n - 1 {
        if (cum[i] as i64) * 10000 < side {
            continue;
        }
        for j in i + 1..n {
            if (total - cum[j] as i64) * 10000 < side {
                continue;
            }
            let w = (cum[j] - cum[i]) as i64;
            let size = (w * 10000 - target_bp as i64 * total).abs();
            let mid = ((cum[i] + cum[j]) as i64 * 10000 - 2 * center_bp as i64 * total).abs();
            let k = (size, mid, i);
            if best.as_ref().is_none_or(|(b, _)| k < *b) {
                best = Some((k, (i, j)));
            }
        }
    }
    best.map(|(_, r)| r)
}

pub fn prefix_prompt(register: &str, prefix: &str, words: usize) -> String {
    format!(
        "The text below is the opening of {register}. Write the rest of it: about {words} more words that carry on directly from where it stops, in the same voice, register and formatting. Do not repeat, quote or summarise the opening, and do not add a title or a closing note. Return only the new text.\n\nOPENING:\n{prefix}"
    )
}

pub fn middle_rewrite_prompt(register: &str, before: &str, span: &str, after: &str, words: usize) -> String {
    format!(
        "Below is {register}, split into BEFORE, MIDDLE and AFTER. Rewrite only the MIDDLE part in your own words, about {words} words long, so that it reads naturally between BEFORE and AFTER and keeps its facts and meaning. Match the voice and formatting of the surrounding text. Return only the new middle part, without BEFORE or AFTER and without any preface or explanation.\n\nBEFORE:\n{before}\n\nMIDDLE:\n{span}\n\nAFTER:\n{after}"
    )
}

pub fn middle_infill_prompt(register: &str, before: &str, after: &str, words: usize, terms: &[String]) -> String {
    let topic = if terms.is_empty() { String::new() } else { format!(" It should touch on: {}.", terms.join(", ")) };
    format!(
        "Below are the beginning and the end of {register}. A passage of about {words} words is missing between them. Write the missing passage so that the text reads continuously from BEFORE to AFTER.{topic} Match the voice and formatting of the surrounding text, and do not repeat BEFORE or AFTER. Return only the missing passage, without any preface or explanation.\n\nBEFORE:\n{before}\n\nAFTER:\n{after}"
    )
}

/// Register phrase for prompts: the row genre's description when the brief
/// library knows it, else the domain's protocol register.
fn register_of(h: &Human) -> String {
    match crate::briefs::genre_desc(&h.genre) {
        Some(g) => format!("a passage of {g}"),
        None => plan::register(h.domain).to_owned(),
    }
}

fn gen_for(split: &str, slot: usize) -> Result<&'static str> {
    Ok(match split {
        "train" => plan::TRAIN_INSTRUCT[slot % plan::TRAIN_INSTRUCT.len()],
        "dev_iter" => plan::FAMILY_ORDER[slot % plan::FAMILY_ORDER.len()],
        s => bail!("plan v3 covers train and dev_iter only, not {s}"),
    })
}

pub struct V3Inputs<'a> {
    pub humans: &'a [Human],
    /// Per-domain Train IDF (infill key terms).
    pub idf: &'a HashMap<&'static str, Idf>,
    /// Splits to plan (`train`, optionally `dev_iter` as a supplementary,
    /// ungated slice).
    pub splits: &'a [String],
    /// A human gets a prefix continuation when rank(k_prefix) mod this == 0.
    pub prefix_every: usize,
    /// A human gets a middle rewrite when rank(k_middle) mod this == 0.
    pub middle_every: usize,
}

pub fn build_v3(inp: &V3Inputs) -> Result<Plan> {
    ensure!(inp.prefix_every >= 1 && inp.middle_every >= 1, "rates must be at least 1");
    for s in inp.splits {
        ensure!(s == "train" || s == "dev_iter", "plan v3 covers train and dev_iter only, not {s}");
    }
    let stop = text::stopwords();
    let hs: Vec<&Human> = inp.humans.iter().filter(|h| inp.splits.contains(&h.split)).collect();
    let names = ["prefix", "prefix_gen", "middle", "middle_gen", "middle_mode"];
    let rk: HashMap<&str, HashMap<String, usize>> = names.iter().map(|x| (*x, ranks(&hs, x))).collect();
    let mut items = Vec::new();
    let mut requests = Vec::new();
    let mut failures = Vec::new();
    for h in &hs {
        let r = |x: &str| rk[x][&h.id];
        let mut ranks_rec = BTreeMap::new();
        for x in names {
            ranks_rec.insert(format!("v3:{x}"), r(x));
        }
        let sents = text::sentences(&h.text);
        let cum = plan::prefix_words(&sents, &h.text);
        ensure!(cum[sents.len()] == h.word_count, "sentence word accounting for {}", h.id);
        let reg = register_of(h);
        let new_item = |cohort: &str, op: &str, generator: &str, tmpl: &str| -> Item {
            let item_id = format!("pos-{}", &text::sha_hex(format!("v3:{}:{}:{}", h.split, h.id, cohort).as_bytes())[..24]);
            let decoding = diverse::sample_decoding(&item_id, generator, false);
            Item {
                item_id,
                split: h.split.clone(),
                domain: h.domain.into(),
                parent_id: h.id.clone(),
                parent_text_sha256: h.text_sha256.clone(),
                source_group: h.source_group.clone(),
                cohort: cohort.into(),
                kind: "span".into(),
                bin: None,
                target_bp: None,
                anchor: None,
                multi_span: false,
                generator: generator.into(),
                decoding,
                template_id: tmpl.into(),
                source_spans: vec![],
                requested_fraction: None,
                requests: vec![],
                notes: vec![],
                ranks: ranks_rec.clone(),
                operation: Some(op.into()),
                style: None,
            }
        };
        let mut push = |mut it: Item, prompt: String, span: (usize, usize), target: usize, style: Value| {
            it.style = Some(style);
            let tmpl = format!("{}:{}", it.template_id, it.operation.clone().unwrap_or_default());
            let rs = plan::request_sha(&it.generator, "chat", &tmpl, &prompt, &it.decoding);
            let req = Request {
                request_id: format!("req-{}", &text::sha_hex(format!("{}:0", it.item_id).as_bytes())[..24]),
                item_id: it.item_id.clone(),
                parent_id: it.parent_id.clone(),
                split: it.split.clone(),
                generator: it.generator.clone(),
                endpoint: "chat".into(),
                template_id: it.template_id.clone(),
                prompt,
                span_index: 0,
                source_span: Some(span),
                target_words: target,
                cut_budget: None,
                decoding: it.decoding.clone(),
                seed: plan::seed_of(&rs),
                request_sha256: rs,
                op: it.operation.clone(),
                cohort: Some(it.cohort.clone()),
                attempt_limit: None,
            };
            it.requests.push(req.request_id.clone());
            requests.push(req);
            items.push(it);
        };
        let n = sents.len();
        if r("prefix") % inp.prefix_every == 0 {
            let g = gen_for(&h.split, r("prefix_gen"))?;
            let mut it = new_item(COHORT_PREFIX, OP_PREFIX, g, PREFIX_TEMPLATE_ID);
            let target = draw_bp("prefix_bp", &it.item_id, PREFIX_MIN_BP, PREFIX_MAX_BP);
            match prefix_cut(&cum, target) {
                Some(j) => {
                    let span = plan::byte_span(&sents, (j, n));
                    let removed = cum[n] - cum[j];
                    it.source_spans = vec![span];
                    it.anchor = Some("end".into());
                    it.requested_fraction = Some(removed as f64 / cum[n] as f64);
                    let kept_bp = (cum[j] as f64 * 10000.0 / cum[n] as f64).round() as u32;
                    let prompt = prefix_prompt(&reg, h.text[..span.0].trim_end(), removed);
                    let st = json!({"style_policy":RULES_ID,"style_kind":"matched_context","operation":OP_PREFIX,"voice_id":null,"brief_id":null,
                        "default_voice":true,"target_prefix_bp":target,"kept_prefix_bp":kept_bp,"kept_prefix_words":cum[j],"kept_prefix_sentences":j,
                        "replaced_words":removed,"source_conditioned":true});
                    push(it, prompt, span, removed, st);
                }
                None => failures.push(json!({"item_id":it.item_id,"split":h.split,"parent_id":h.id,"cohort":it.cohort,"generator":g,
                    "operation":OP_PREFIX,"stage":"plan","reason":"no_whole_sentence_prefix_in_30_70_percent","sentences":n,"target_prefix_bp":target})),
            }
        }
        if r("middle") % inp.middle_every == 0 {
            let g = gen_for(&h.split, r("middle_gen"))?;
            let op = if r("middle_mode") % 2 == 0 { OP_MIDDLE_REWRITE } else { OP_MIDDLE_INFILL };
            let mut it = new_item(COHORT_MIDDLE, op, g, MIDDLE_TEMPLATE_ID);
            let target = draw_bp("middle_bp", &it.item_id, MIDDLE_MIN_BP, MIDDLE_MAX_BP);
            let center = draw_bp("middle_center", &it.item_id, MIDDLE_CENTER_MIN_BP, MIDDLE_CENTER_MAX_BP);
            match middle_run(&cum, target, center) {
                Some(run) => {
                    let span = plan::byte_span(&sents, run);
                    let removed = cum[run.1] - cum[run.0];
                    it.source_spans = vec![span];
                    it.anchor = Some("middle".into());
                    it.target_bp = Some(target);
                    it.requested_fraction = Some(removed as f64 / cum[n] as f64);
                    let before = h.text[..span.0].trim_end();
                    let after = h.text[span.1..].trim_start();
                    let span_text = &h.text[span.0..span.1];
                    let terms = if op == OP_MIDDLE_INFILL { text::key_terms(span_text, &inp.idf[h.domain], &stop, INFILL_TERMS) } else { vec![] };
                    let prompt = if op == OP_MIDDLE_REWRITE {
                        middle_rewrite_prompt(&reg, before, span_text, after, removed)
                    } else {
                        middle_infill_prompt(&reg, before, after, removed, &terms)
                    };
                    let st = json!({"style_policy":RULES_ID,"style_kind":"matched_context","operation":op,"voice_id":null,"brief_id":null,
                        "default_voice":true,"target_run_bp":target,"target_center_bp":center,"run_sentences":[run.0, run.1],
                        "kept_before_words":cum[run.0],"kept_after_words":cum[n] - cum[run.1],"replaced_words":removed,
                        "removed_span_shown":op == OP_MIDDLE_REWRITE,"infill_terms":terms,"source_conditioned":true});
                    push(it, prompt, span, removed, st);
                }
                None => failures.push(json!({"item_id":it.item_id,"split":h.split,"parent_id":h.id,"cohort":it.cohort,"generator":g,
                    "operation":op,"stage":"plan","reason":"no_interior_whole_sentence_run_with_kept_context","sentences":n,"target_run_bp":target})),
            }
        }
    }
    Ok(Plan { items, requests, failures })
}

/// Round-2 plan key (V21).
pub fn r2_key(x: &str, id: &str) -> String {
    text::sha_hex(format!("{R2_PLAN_KEY_PREFIX}{x}:{id}").as_bytes())
}

/// Ranks of the V21 key `x` within (split, domain), ascending key order.
pub fn r2_ranks(humans: &[&Human], x: &str) -> HashMap<String, usize> {
    let mut by: BTreeMap<(&str, &str), Vec<(String, &str)>> = BTreeMap::new();
    for h in humans {
        by.entry((h.split.as_str(), h.domain)).or_default().push((r2_key(x, &h.id), h.id.as_str()));
    }
    let mut out = HashMap::new();
    for (_, mut v) in by {
        v.sort();
        for (r, (_, id)) in v.into_iter().enumerate() {
            out.insert(id.to_owned(), r);
        }
    }
    out
}

/// Validated `--base-generators`: Train base families only, no repeats,
/// returned in `plan::TRAIN_BASE` order so the assignment does not depend on
/// the order they were listed in.
pub fn base_generators(list: &[String]) -> Result<Vec<&'static str>> {
    ensure!(!list.is_empty(), "base-model mode needs at least one base generator");
    for g in list {
        ensure!(plan::TRAIN_BASE.contains(&g.as_str()), "{g} is not a Train base family (allowed: {:?})", plan::TRAIN_BASE);
    }
    let set: BTreeSet<&str> = list.iter().map(String::as_str).collect();
    ensure!(set.len() == list.len(), "a base generator is listed twice: {list:?}");
    Ok(plan::TRAIN_BASE.iter().copied().filter(|g| set.contains(g)).collect())
}

/// Inputs instruct-mode `plan-v3` reads: Train (IDF and the planned Train
/// humans), Dev-iter (only as a planned split) and CAL (only as `generate`'s
/// overlap extras). Old Dev-select is never read, scored or parsed (protocol
/// A2 §2.4) and no v3 split uses it, so `--dev-select` is refused.
pub const PLAN_V3_INPUTS: [&str; 3] = ["train", "dev_iter", "cal"];
/// Inputs base-model mode reads: it plans Train only, so Dev-iter is refused
/// too. CAL stays an overlap-extras input only.
pub const PLAN_V3_BASE_INPUTS: [&str; 2] = ["train", "cal"];

/// Checks `plan-v3`'s arguments before any file is opened. `given` names the
/// input flags that were passed (`train`, `dev_iter`, `dev_select`, `cal`).
/// Refuses `--dev-select` in both modes, every input other than Train and CAL
/// in base-model mode, planned splits other than Train (base-model mode) or
/// Train and Dev-iter (instruct mode), a planned split without its input, and
/// the instruct rates in base-model mode. Returns the validated base
/// generators in `plan::TRAIN_BASE` order (empty in instruct mode).
pub fn check_plan_v3_args(
    given: &[&str],
    splits: &[String],
    prefix_every: Option<usize>,
    middle_every: Option<usize>,
    base_list: &[String],
) -> Result<Vec<&'static str>> {
    let base = !base_list.is_empty();
    ensure!(
        !given.contains(&"dev_select"),
        "plan v3 refuses --dev-select: old Dev-select is never read, scored or parsed (protocol A2 section 2.4) and no v3 split uses it"
    );
    ensure!(given.contains(&"train"), "plan v3 needs --train");
    let allowed: &[&str] = if base { &PLAN_V3_BASE_INPUTS } else { &PLAN_V3_INPUTS };
    for g in given {
        ensure!(allowed.contains(g), "base-model mode plans Train only and refuses --{}", g.replace('_', "-"));
    }
    let gens = if base { base_generators(base_list)? } else { Vec::new() };
    if base {
        ensure!(splits == ["train"], "base-model mode plans Train only (--splits train), not {splits:?}");
        ensure!(
            prefix_every.is_none() && middle_every.is_none(),
            "--prefix-every and --middle-every select instruct cohorts; base-model mode plans none"
        );
    }
    for s in splits {
        ensure!(s == "train" || s == "dev_iter", "plan v3 covers train and dev_iter only, not {s}");
        ensure!(given.contains(&s.as_str()), "planned split {s} has no input");
    }
    Ok(gens)
}

/// Whether `generate`, `finalize` and `replan-check` read a recorded input of
/// a plan with this schema. A v3 plan never reads a recorded `dev_select`
/// input: plans written before the refusal recorded one, and no v3 item,
/// overlap extra or IDF uses it (protocol A2 §2.4).
pub fn plan_reads_input(schema: Option<&str>, split: &str) -> bool {
    !(schema == Some(PLAN_V3_SCHEMA) && split == "dev_select")
}

/// Generator and kind of base-mode rank `r` over `gens` (G families):
/// generator `gens[r mod G]`; continuation when `floor(r / G) mod 2 == 0`,
/// else splice. Within a (split, domain) each family's ranks are consecutive
/// multiples of G apart, so its kinds alternate: continuation and splice are
/// 1:1 (to within one) in every domain x generator cell.
pub fn base_slot(r: usize, gens: &[&'static str]) -> (&'static str, bool) {
    (gens[r % gens.len()], (r / gens.len()).is_multiple_of(2))
}

/// Base-model mode: one `train_base_v3` item per Train human (other splits
/// are not planned), with one `/completion` request under the Tier C
/// robustness rules of `plan::build_test`: `plan::base_runs` spans (a
/// continuation keeps the shortest whole-sentence prefix with at least 30% of
/// the words; a splice keeps the shortest whole-sentence prefix and suffix
/// with at least 25% each), the verbatim prefix as the prompt with no
/// template, cut budget and target equal to the removed words, and
/// `plan::robustness_decoding`. A passage without such a plan is a recorded
/// plan failure, never re-planned with another kind or generator. The
/// per-family cohort size is chosen afterwards by `stratify-train`.
pub fn build_v3_base(humans: &[Human], gens: &[&'static str]) -> Result<Plan> {
    ensure!(!gens.is_empty(), "no base generators");
    for g in gens {
        ensure!(plan::TRAIN_BASE.contains(g), "{g} is not a Train base family");
    }
    let hs: Vec<&Human> = humans.iter().filter(|h| h.split == "train").collect();
    let rk = r2_ranks(&hs, BASE_KEY);
    let dec = plan::robustness_decoding();
    let mut items = Vec::new();
    let mut requests = Vec::new();
    let mut failures = Vec::new();
    for h in &hs {
        let r = rk[&h.id];
        let (g, cont) = base_slot(r, gens);
        let kind = if cont { "continuation" } else { "splice" };
        let tmpl = if cont { plan::BASE_CONT_TEMPLATE_ID } else { plan::BASE_SPLICE_TEMPLATE_ID };
        let item_id = format!("pos-{}", &text::sha_hex(format!("v3:{}:{}:{}", h.split, h.id, COHORT_BASE).as_bytes())[..24]);
        let sents = text::sentences(&h.text);
        let cum = plan::prefix_words(&sents, &h.text);
        let n = sents.len();
        ensure!(cum[n] == h.word_count, "sentence word accounting for {}", h.id);
        let Some((a, b)) = plan::base_runs(&cum, cont) else {
            failures.push(json!({"item_id":item_id,"split":h.split,"parent_id":h.id,"cohort":COHORT_BASE,"generator":g,"kind":kind,
                "stage":"plan","reason":"no_valid_prefix_suffix_plan","sentences":n}));
            continue;
        };
        let (s, e) = (sents[a].start, sents[b - 1].end);
        let removed = text::words(&h.text[s..e]);
        let prompt = h.text[..sents[a - 1].end].to_owned();
        let rs = plan::request_sha(g, "completion", tmpl, &prompt, &dec);
        let req = Request {
            request_id: format!("req-{}", &text::sha_hex(format!("{item_id}:0").as_bytes())[..24]),
            item_id: item_id.clone(),
            parent_id: h.id.clone(),
            split: h.split.clone(),
            generator: g.into(),
            endpoint: "completion".into(),
            template_id: tmpl.into(),
            prompt,
            span_index: 0,
            source_span: Some((s, e)),
            target_words: removed,
            cut_budget: Some(removed),
            decoding: dec.clone(),
            seed: plan::seed_of(&rs),
            request_sha256: rs,
            op: None,
            cohort: Some(COHORT_BASE.into()),
            attempt_limit: None,
        };
        let style = json!({"style_policy":BASE_RULES_ID,"style_kind":"base_checkpoint","operation":kind,"voice_id":null,"brief_id":null,
            "default_voice":true,"prompt":"verbatim_prefix_no_template","kept_prefix_words":cum[a],"kept_prefix_sentences":a,
            "kept_suffix_words":cum[n] - cum[b],"replaced_sentences":[a, b],"replaced_words":removed,"source_conditioned":true});
        items.push(Item {
            item_id,
            split: h.split.clone(),
            domain: h.domain.into(),
            parent_id: h.id.clone(),
            parent_text_sha256: h.text_sha256.clone(),
            source_group: h.source_group.clone(),
            cohort: COHORT_BASE.into(),
            kind: kind.into(),
            bin: None,
            target_bp: None,
            anchor: None,
            multi_span: false,
            generator: g.into(),
            decoding: dec.clone(),
            template_id: tmpl.into(),
            source_spans: vec![(s, e)],
            requested_fraction: Some(removed as f64 / h.word_count as f64),
            requests: vec![req.request_id.clone()],
            notes: vec![],
            ranks: BTreeMap::from([(format!("r2:{BASE_KEY}"), r)]),
            operation: None,
            style: Some(style),
        });
        requests.push(req);
    }
    Ok(Plan { items, requests, failures })
}

/// Extra acceptance screens for the v3 operations, given the cleaned
/// response, the parent text and the replaced byte span. A response must not
/// repeat a 12-word window of the kept human context (prefix or suffix);
/// when the removed run was not shown (continuation, infill) it must not
/// reproduce a 12-word window of it either, since such words would be
/// recalled human text labelled as model text.
pub fn screen(op: &str, clean: &str, parent: &str, span: (usize, usize), diag: &mut Value) -> Vec<String> {
    let mut reasons = Vec::new();
    if !OPS.contains(&op) {
        return reasons;
    }
    let (s, e) = span;
    let kept = crate::overlap::shares_window(clean, &parent[..s], COPY_WINDOW) || crate::overlap::shares_window(clean, &parent[e..], COPY_WINDOW);
    let removed = crate::overlap::shares_window(clean, &parent[s..e], COPY_WINDOW);
    diag["shares_kept_context_12_word_window"] = json!(kept);
    diag["shares_removed_run_12_word_window"] = json!(removed);
    if kept {
        reasons.push("copies_kept_human_context_12_word_window".into());
    }
    if removed && op != OP_MIDDLE_REWRITE {
        reasons.push("copies_removed_human_text_12_word_window".into());
    }
    reasons
}

/// Exact per-word labels (0 human, 1 model) under the word policy
/// (`unicode_split_whitespace_v1`): a word is model-written when its bytes
/// lie inside an AI span. Errors when a span cuts a word.
pub fn word_labels(text: &str, ai_spans: &[(usize, usize)]) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    let mut pos = 0;
    for w in text.split_whitespace() {
        let st = pos + text[pos..].find(w).unwrap();
        let en = st + w.len();
        pos = en;
        let inside = ai_spans.iter().any(|&(a, b)| a <= st && en <= b);
        let cut = ai_spans.iter().any(|&(a, b)| (a > st && a < en) || (b > st && b < en));
        ensure!(!cut, "AI span cuts the word at bytes {st}..{en}");
        out.push(inside as u8);
    }
    Ok(out)
}

/// Run-length form of [`word_labels`]: `[[label, words], ...]`.
pub fn label_runs(labels: &[u8]) -> Vec<(u8, usize)> {
    let mut v: Vec<(u8, usize)> = Vec::new();
    for &l in labels {
        match v.last_mut() {
            Some((x, n)) if *x == l => *n += 1,
            _ => v.push((l, 1)),
        }
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::generation;

    fn cum(ws: &[usize]) -> Vec<usize> {
        let mut c = vec![0];
        for w in ws {
            c.push(c.last().unwrap() + w);
        }
        c
    }

    /// Synthetic passage: `n` sentences of `w` words each.
    fn passage(n: usize, w: usize) -> String {
        (0..n)
            .map(|i| {
                let body: Vec<String> = (0..w).map(|k| format!("w{i}x{k}")).collect();
                format!("{}.", body.join(" "))
            })
            .collect::<Vec<_>>()
            .join(" ")
    }

    fn human(id: &str, text: &str, split: &str) -> Human {
        Human {
            split: split.into(),
            domain: "fineweb",
            id: id.into(),
            text: text.into(),
            text_sha256: text::sha_hex(text.as_bytes()),
            word_count: text::words(text),
            title: None,
            source_group: format!("g-{id}"),
            genre: "web".into(),
        }
    }

    #[test]
    fn prefix_cut_stays_in_30_70_and_tracks_target() {
        let c = cum(&[10; 10]);
        assert_eq!(prefix_cut(&c, 3000), Some(3));
        assert_eq!(prefix_cut(&c, 7000), Some(7));
        assert_eq!(prefix_cut(&c, 5400), Some(5));
        // tie between 50% and 60% for 55%: shorter prefix
        assert_eq!(prefix_cut(&c, 5500), Some(5));
        // lopsided: no boundary inside 30-70%
        assert_eq!(prefix_cut(&cum(&[5, 95]), 5000), None);
        assert_eq!(prefix_cut(&cum(&[100]), 5000), None);
        for t in [3000, 4100, 5000, 6900, 7000] {
            let j = prefix_cut(&c, t).unwrap();
            assert!(c[j] * 10000 >= 3000 * 100 && c[j] * 10000 <= 7000 * 100);
        }
    }

    #[test]
    fn middle_run_is_interior_with_context() {
        let c = cum(&[10; 10]);
        let (i, j) = middle_run(&c, 3000, 5000).unwrap();
        assert_eq!(j - i, 3);
        assert!(i >= 1 && j <= 9);
        assert_eq!((i, j), (3, 6)); // midpoint 45 vs 50 ties with (4,7) at 55: earlier run
        let (i, j) = middle_run(&c, 2000, 3000).unwrap();
        assert_eq!((i, j), (2, 4));
        // every candidate keeps >= 10% each side
        let (i, j) = middle_run(&c, 9000, 5000).unwrap();
        assert!(c[i] >= 10 && 100 - c[j] >= 10);
        assert_eq!(middle_run(&cum(&[10, 10]), 3000, 5000), None);
        // sides too small: first and last sentences hold under 10%
        assert_eq!(middle_run(&cum(&[1, 98, 1]), 3000, 5000), None);
    }

    #[test]
    fn draws_are_keyed_and_in_range() {
        for k in 0..300 {
            let id = format!("pos-{k}");
            let p = draw_bp("prefix_bp", &id, PREFIX_MIN_BP, PREFIX_MAX_BP);
            assert!((PREFIX_MIN_BP..=PREFIX_MAX_BP).contains(&p));
            assert_eq!(p, draw_bp("prefix_bp", &id, PREFIX_MIN_BP, PREFIX_MAX_BP));
        }
        assert_ne!(key("prefix", "a"), crate::diverse::key("prefix", "a"));
    }

    #[test]
    fn plan_builds_both_cohorts_with_exact_spans() {
        let texts: Vec<String> = (0..8).map(|k| passage(6 + k, 9)).collect();
        let hs: Vec<Human> = texts.iter().enumerate().map(|(k, t)| human(&format!("h{k}"), t, "train")).collect();
        let stop = text::stopwords();
        let mut idf = HashMap::new();
        let mut d = Idf::default();
        for h in &hs {
            d.add(&h.text, &stop);
        }
        idf.insert("fineweb", d);
        let splits = vec!["train".to_string()];
        let p = build_v3(&V3Inputs { humans: &hs, idf: &idf, splits: &splits, prefix_every: 1, middle_every: 1 }).unwrap();
        assert!(p.failures.is_empty(), "{:?}", p.failures);
        assert_eq!(p.items.len(), 16);
        assert_eq!(p.requests.len(), 16);
        let modes: std::collections::BTreeSet<&str> = p.items.iter().filter_map(|i| i.operation.as_deref()).collect();
        assert!(modes.contains(OP_PREFIX) && modes.contains(OP_MIDDLE_REWRITE) && modes.contains(OP_MIDDLE_INFILL));
        let by: HashMap<&str, &Human> = hs.iter().map(|h| (h.id.as_str(), h)).collect();
        for (it, rq) in p.items.iter().zip(&p.requests) {
            assert_eq!(rq.item_id, it.item_id);
            assert!(plan::TRAIN_INSTRUCT.contains(&it.generator.as_str()));
            let h = by[it.parent_id.as_str()];
            let (s, e) = it.source_spans[0];
            assert_eq!(rq.source_span, Some((s, e)));
            assert_eq!(rq.target_words, text::words(&h.text[s..e]));
            let before = text::words(&h.text[..s]);
            let after = text::words(&h.text[e..]);
            let total = h.word_count;
            match it.cohort.as_str() {
                COHORT_PREFIX => {
                    assert_eq!(e, h.text.len());
                    assert!(before * 10 >= 3 * total && before * 10 <= 7 * total);
                    assert!(rq.prompt.contains(h.text[..s].trim_end()) && !rq.prompt.contains(&h.text[s..e]));
                }
                COHORT_MIDDLE => {
                    assert!(before * 10 >= total && after * 10 >= total && s > 0 && e < h.text.len());
                    let shown = rq.prompt.contains(&h.text[s..e]);
                    assert_eq!(shown, it.operation.as_deref() == Some(OP_MIDDLE_REWRITE));
                }
                c => panic!("cohort {c}"),
            }
            // a synthetic response assembles with exact labels
            let rep = "Generated words stand in for the model here.";
            let (fin, spans, share) = generation::assemble(&h.text, &it.source_spans, &[rep]).unwrap();
            let labels = word_labels(&fin, &spans).unwrap();
            let rw = text::words(rep);
            let runs = label_runs(&labels);
            if it.cohort == COHORT_PREFIX {
                assert_eq!(runs, vec![(0, before), (1, rw)]);
            } else {
                assert_eq!(runs, vec![(0, before), (1, rw), (0, after)]);
            }
            assert!((share - rw as f64 / (before + after + rw) as f64).abs() < 1e-12);
        }
        // deterministic and disjoint from plan v2 ids
        let q = build_v3(&V3Inputs { humans: &hs, idf: &idf, splits: &splits, prefix_every: 1, middle_every: 1 }).unwrap();
        let a: Vec<&str> = p.requests.iter().map(|r| r.request_sha256.as_str()).collect();
        let b: Vec<&str> = q.requests.iter().map(|r| r.request_sha256.as_str()).collect();
        assert_eq!(a, b);
        let v2_id = format!("pos-{}", &text::sha_hex(format!("v2:train:h0:{COHORT_PREFIX}").as_bytes())[..24]);
        assert!(p.items.iter().all(|i| i.item_id != v2_id));
    }

    #[test]
    fn plan_rejects_sealed_splits_and_records_failures() {
        let hs = vec![human("short", "One sentence only here.", "train")];
        let idf = HashMap::from([("fineweb", Idf::default())]);
        let bad = vec!["cal".to_string()];
        assert!(build_v3(&V3Inputs { humans: &hs, idf: &idf, splits: &bad, prefix_every: 1, middle_every: 1 }).is_err());
        let ok = vec!["train".to_string()];
        let p = build_v3(&V3Inputs { humans: &hs, idf: &idf, splits: &ok, prefix_every: 1, middle_every: 1 }).unwrap();
        assert!(p.items.is_empty());
        assert_eq!(p.failures.len(), 2);
    }

    #[test]
    fn screens_reject_context_echo_and_recall() {
        let p = passage(6, 14);
        let sents = text::sentences(&p);
        let span = plan::byte_span(&sents, (2, 4));
        let fresh = "Completely fresh words that never appeared anywhere in the parent passage at all today.";
        let mut d = json!({});
        assert!(screen(OP_PREFIX, fresh, &p, span, &mut d).is_empty());
        let echo = format!("{} and more", &p[..sents[1].end]);
        assert_eq!(screen(OP_MIDDLE_INFILL, &echo, &p, span, &mut d), vec!["copies_kept_human_context_12_word_window"]);
        let recall = p[span.0..span.1].to_string();
        assert_eq!(screen(OP_MIDDLE_INFILL, &recall, &p, span, &mut d), vec!["copies_removed_human_text_12_word_window"]);
        // a shown run may be partly kept by a rewrite: recorded, not rejected
        assert!(screen(OP_MIDDLE_REWRITE, &recall, &p, span, &mut d).is_empty());
        assert_eq!(d["shares_removed_run_12_word_window"], true);
        assert!(screen("paraphrase", &echo, &p, span, &mut d).is_empty());
    }

    /// Synthetic vocabulary for the fixture passages (no corpus text).
    const SYN: [&str; 24] = [
        "amber", "basalt", "cobalt", "delta", "ember", "fjord", "garnet", "harbor", "indigo", "juniper", "kestrel", "lantern", "meadow",
        "nectar", "orchid", "pebble", "quartz", "river", "saffron", "timber", "umber", "velvet", "willow", "zephyr",
    ];

    /// Synthetic passage: `n` sentences of varied length from `seed`, with a
    /// blank-line paragraph break after every fourth sentence.
    fn synth(seed: usize, n: usize) -> String {
        let mut out = String::new();
        for i in 0..n {
            let len = 5 + (seed * 7 + i * 13) % 17;
            let ws: Vec<&str> = (0..len).map(|k| SYN[(seed * 31 + i * 17 + k * 7) % SYN.len()]).collect();
            let s = ws.join(" ");
            if i > 0 {
                out.push_str(if i % 4 == 0 { "\n\n" } else { " " });
            }
            out.push_str(&s[..1].to_uppercase());
            out.push_str(&s[1..]);
            out.push('.');
        }
        out
    }

    fn synth_human(split: &str, domain: &'static str, k: usize, n: usize) -> Human {
        let text = synth(k, n);
        Human {
            split: split.into(),
            domain,
            id: format!("{split}-{domain}-{k}"),
            text_sha256: text::sha_hex(text.as_bytes()),
            word_count: text::words(&text),
            text,
            title: None,
            source_group: format!("g-{domain}-{}", k / 2),
            genre: if domain == "wikipedia" { "encyclopedia".into() } else { "web".into() },
        }
    }

    /// Fixture: Train humans in three domains and Dev-iter humans in two,
    /// with one- to fourteen-sentence passages (short ones give plan failures).
    fn fixture() -> Vec<Human> {
        let mut hs = Vec::new();
        for (d, base) in [("fineweb", 0usize), ("wikipedia", 40), ("reddit", 80)] {
            for k in 0..14 {
                hs.push(synth_human("train", d, base + k, 1 + k));
            }
        }
        for (d, base) in [("fineweb", 200usize), ("wikipedia", 240)] {
            for k in 0..6 {
                hs.push(synth_human("dev_iter", d, base + k, 2 + 2 * k));
            }
        }
        hs
    }

    fn fixture_idf(hs: &[Human]) -> HashMap<&'static str, Idf> {
        let stop = text::stopwords();
        let mut idf: HashMap<&'static str, Idf> = plan::DOMAINS.iter().map(|d| (*d, Idf::default())).collect();
        for h in hs.iter().filter(|h| h.split == "train") {
            idf.get_mut(h.domain).unwrap().add(&h.text, &stop);
        }
        idf
    }

    /// SHA-256 of rows serialised as `write_jsonl` writes them.
    fn rows_sha<T: serde::Serialize>(rows: &[T]) -> String {
        let mut b = Vec::new();
        for r in rows {
            serde_json::to_writer(&mut b, r).unwrap();
            b.push(b'\n');
        }
        text::sha_hex(&b)
    }

    /// Instruct-mode plan v3 is unchanged by base-model mode: items, requests
    /// and plan failures serialise to the digests the code at origin/main
    /// `f2b44852` (before B6) produced for this fixture.
    #[test]
    fn plan_v3_instruct_output_matches_pre_b6_digests() {
        let hs = fixture();
        let idf = fixture_idf(&hs);
        let want = [
            (38, 38, 4, "7c3f981170f625923a21448ba4b367e7577008c7bd33d97543b3489089764dee",
                "91e6704edff5c63e91d88dea802840caf46fcd35cc4c72d7c570e03276efdeea",
                "f78af94f8447c8c80fb28497318b49215c7a184623fa112b27c0bcfb5838b493"),
            (97, 97, 11, "8a5ab5e825863563dda6e4976e2f65d160c3e9ef94224edc3e4736a8ac0c3229",
                "986a7eeb2198c3891957230e50991506328f5231b10e41e26d8462b8f8756633",
                "a50ebeda664bb2ddbc265e34e16eacc9340dd08fd5ba00a14d644525c68cbbd0"),
            (32, 32, 4, "016ab3272d70b4c466955d3ecbc75e15dace7a84829db58d1a5d586143a74d0f",
                "20f82aa38dff330efe06ff6ab6df0980aa66cc4188bc995dc1335bc94d7cee20",
                "f78af94f8447c8c80fb28497318b49215c7a184623fa112b27c0bcfb5838b493"),
        ];
        let runs = [(vec!["train".to_string()], 2, 2), (vec!["train".to_string(), "dev_iter".to_string()], 1, 1), (vec!["train".to_string()], 3, 2)];
        for ((splits, pe, me), w) in runs.iter().zip(want) {
            let p = build_v3(&V3Inputs { humans: &hs, idf: &idf, splits, prefix_every: *pe, middle_every: *me }).unwrap();
            let got = (p.items.len(), p.requests.len(), p.failures.len(), rows_sha(&p.items), rows_sha(&p.requests), rows_sha(&p.failures));
            assert_eq!((got.0, got.1, got.2, got.3.as_str(), got.4.as_str(), got.5.as_str()), w, "{splits:?} {pe} {me}");
            assert!(p.items.iter().all(|i| i.cohort != COHORT_BASE && plan::TRAIN_INSTRUCT.contains(&i.generator.as_str()) || i.split == "dev_iter"));
        }
    }

    fn all_base() -> Vec<&'static str> {
        base_generators(&plan::TRAIN_BASE.iter().map(|g| g.to_string()).collect::<Vec<_>>()).unwrap()
    }

    #[test]
    fn base_generators_are_train_base_only_in_fixed_order() {
        let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        assert_eq!(base_generators(&s(&["ernie_base", "granite_base"])).unwrap(), vec!["granite_base", "ernie_base"]);
        assert_eq!(all_base(), plan::TRAIN_BASE.to_vec());
        assert!(base_generators(&[]).is_err());
        for bad in [plan::TEST_ONLY_BASE, "qwen36", "granite_instruct", plan::CAL_ONLY, "granite"] {
            assert!(base_generators(&s(&[bad])).is_err(), "{bad}");
        }
        assert!(base_generators(&s(&["smollm3_base", "smollm3_base"])).is_err());
        assert!(build_v3_base(&fixture(), &["olmo2_base"]).is_err());
    }

    /// `plan-v3`'s argument checks: `--dev-select` is refused in both modes,
    /// base-model mode reads Train (and CAL) only, and every planned split
    /// needs its input.
    #[test]
    fn plan_v3_args_refuse_dev_select_and_non_train_base_inputs() {
        let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        let train = s(&["train"]);
        let none: Vec<String> = Vec::new();
        let gens = s(&["ernie_base", "granite_base"]);
        let err = |r: Result<Vec<&'static str>>| r.unwrap_err().to_string();
        // instruct mode
        assert_eq!(check_plan_v3_args(&["train"], &train, None, None, &none).unwrap(), Vec::<&str>::new());
        assert!(check_plan_v3_args(&["train", "dev_iter", "cal"], &s(&["train", "dev_iter"]), Some(2), Some(3), &none).is_ok());
        assert!(check_plan_v3_args(&["train", "cal"], &train, Some(1), None, &none).is_ok());
        for given in [&["train", "dev_select"][..], &["train", "dev_iter", "dev_select", "cal"][..], &["dev_select"][..]] {
            assert!(err(check_plan_v3_args(given, &train, None, None, &none)).contains("refuses --dev-select"), "{given:?}");
        }
        assert!(err(check_plan_v3_args(&["train"], &s(&["train", "dev_iter"]), None, None, &none)).contains("planned split dev_iter has no input"));
        for bad in ["dev_select", "cal", "test"] {
            assert!(check_plan_v3_args(&["train", "dev_iter", "cal"], &s(&["train", bad]), None, None, &none).is_err(), "{bad}");
        }
        assert!(err(check_plan_v3_args(&["cal"], &none, None, None, &none)).contains("needs --train"));
        // base-model mode
        assert_eq!(check_plan_v3_args(&["train"], &train, None, None, &gens).unwrap(), vec!["granite_base", "ernie_base"]);
        assert!(check_plan_v3_args(&["train", "cal"], &train, None, None, &gens).is_ok());
        assert!(err(check_plan_v3_args(&["train", "dev_select"], &train, None, None, &gens)).contains("refuses --dev-select"));
        assert!(err(check_plan_v3_args(&["train", "dev_iter"], &train, None, None, &gens)).contains("refuses --dev-iter"));
        assert!(err(check_plan_v3_args(&["train", "dev_iter"], &s(&["train", "dev_iter"]), None, None, &gens)).contains("refuses --dev-iter"));
        assert!(err(check_plan_v3_args(&["train"], &s(&["train", "dev_iter"]), None, None, &gens)).contains("plans Train only"));
        assert!(err(check_plan_v3_args(&["train"], &none, None, None, &gens)).contains("plans Train only"));
        assert!(check_plan_v3_args(&["train"], &train, Some(2), None, &gens).is_err());
        assert!(check_plan_v3_args(&["train"], &train, None, Some(2), &gens).is_err());
        assert!(check_plan_v3_args(&["train"], &train, None, None, &s(&["qwen36"])).is_err());
        // recorded inputs a plan's readers open
        for sp in plan::ALL_SPLITS {
            assert!(plan_reads_input(None, sp) && plan_reads_input(Some(plan::PLAN_SCHEMA), sp) && plan_reads_input(Some(diverse::PLAN_V2_SCHEMA), sp));
            assert_eq!(plan_reads_input(Some(PLAN_V3_SCHEMA), sp), sp != "dev_select", "{sp}");
        }
    }

    #[test]
    fn base_slot_rotates_generators_and_alternates_kinds() {
        let g = all_base();
        let got: Vec<(&str, bool)> = (0..8).map(|r| base_slot(r, &g)).collect();
        assert_eq!(got, vec![
            ("granite_base", true), ("smollm3_base", true), ("ernie_base", true),
            ("granite_base", false), ("smollm3_base", false), ("ernie_base", false),
            ("granite_base", true), ("smollm3_base", true),
        ]);
        let one = ["smollm3_base"];
        assert_eq!((0..4).map(|r| base_slot(r, &one).1).collect::<Vec<_>>(), vec![true, false, true, false]);
        // 1:1 within every generator for any run of consecutive ranks
        for n in 0..40 {
            for gi in &g {
                let ks: Vec<bool> = (0..n).map(|r| base_slot(r, &g)).filter(|x| x.0 == *gi).map(|x| x.1).collect();
                let c = ks.iter().filter(|k| **k).count();
                assert!(c.abs_diff(ks.len() - c) <= 1, "n {n} {gi}");
            }
        }
        assert_eq!(r2_key("base_cohort", "x"), text::sha_hex(b"slop_ninja_a_comp_r2_plan_v1:base_cohort:x"));
    }

    #[test]
    fn base_mode_plans_tier_c_items_with_exact_spans() {
        let hs = fixture();
        let gens = all_base();
        let p = build_v3_base(&hs, &gens).unwrap();
        let train: Vec<&Human> = hs.iter().filter(|h| h.split == "train").collect();
        // one assignment (item or recorded failure) per Train human, none for other splits
        assert_eq!(p.items.len() + p.failures.len(), train.len());
        assert_eq!(p.items.len(), p.requests.len());
        assert!(!p.failures.is_empty() && p.failures.iter().all(|f| f["reason"] == "no_valid_prefix_suffix_plan" && f["split"] == "train"));
        let mut parents: Vec<&str> = p.items.iter().map(|i| i.parent_id.as_str()).chain(p.failures.iter().map(|f| f["parent_id"].as_str().unwrap())).collect();
        parents.sort();
        let mut want: Vec<&str> = train.iter().map(|h| h.id.as_str()).collect();
        want.sort();
        assert_eq!(parents, want);
        let by: HashMap<&str, &Human> = hs.iter().map(|h| (h.id.as_str(), h)).collect();
        let rk = r2_ranks(&train, BASE_KEY);
        let dec = plan::robustness_decoding();
        let mut kinds = [0usize; 2];
        for (it, rq) in p.items.iter().zip(&p.requests) {
            let h = by[it.parent_id.as_str()];
            let (g, cont) = base_slot(rk[&h.id], &gens);
            assert_eq!(it.ranks[&format!("r2:{BASE_KEY}")], rk[&h.id]);
            assert_eq!((it.generator.as_str(), it.kind.as_str()), (g, if cont { "continuation" } else { "splice" }));
            assert_eq!((it.cohort.as_str(), it.split.as_str(), rq.cohort.as_deref(), rq.op.as_deref()), (COHORT_BASE, "train", Some(COHORT_BASE), None));
            assert_eq!(it.template_id, if cont { plan::BASE_CONT_TEMPLATE_ID } else { plan::BASE_SPLICE_TEMPLATE_ID });
            assert_eq!((rq.item_id.as_str(), rq.endpoint.as_str(), rq.template_id.as_str()), (it.item_id.as_str(), "completion", it.template_id.as_str()));
            assert_eq!((&it.decoding, &rq.decoding), (&dec, &dec));
            assert_eq!(rq.request_sha256, plan::request_sha(g, "completion", &it.template_id, &rq.prompt, &dec));
            assert_eq!(rq.seed, plan::seed_of(&rq.request_sha256));
            let (s, e) = it.source_spans[0];
            assert_eq!(rq.source_span, Some((s, e)));
            let removed = text::words(&h.text[s..e]);
            assert_eq!((rq.target_words, rq.cut_budget), (removed, Some(removed)));
            // verbatim prefix, no template: the passage up to the last kept sentence
            assert!(h.text.starts_with(&rq.prompt) && rq.prompt.len() <= s && h.text[rq.prompt.len()..s].trim().is_empty());
            assert_eq!(rq.prompt.trim_end(), rq.prompt);
            let sents = text::sentences(&h.text);
            let a = sents.iter().position(|x| x.start == s).unwrap();
            let total = h.word_count;
            let pw = text::words(&rq.prompt);
            let need = if cont { 30 } else { 25 };
            assert!(pw * 100 >= need * total && text::words(&h.text[..sents[a - 1].start]) * 100 < need * total, "shortest prefix");
            let after = text::words(&h.text[e..]);
            if cont {
                assert_eq!(after, 0);
                assert_eq!(e, sents.last().unwrap().end);
            } else {
                let b = sents.iter().position(|x| x.end == e).unwrap();
                assert!(after * 100 >= 25 * total && text::words(&h.text[sents[b + 1].end..]) * 100 < 25 * total, "shortest suffix");
                assert!(removed > 0 && e < h.text.len());
            }
            assert_eq!(it.style.as_ref().unwrap()["style_kind"], "base_checkpoint");
            // exact spans: a cut base response assembles with exact word labels
            let rep = "Generated words stand in for the base model here.";
            let (fin, spans, share) = generation::assemble(&h.text, &it.source_spans, &[rep]).unwrap();
            let runs = label_runs(&word_labels(&fin, &spans).unwrap());
            let rw = text::words(rep);
            assert_eq!(runs, if cont { vec![(0, pw), (1, rw)] } else { vec![(0, pw), (1, rw), (0, after)] });
            assert!((share - rw as f64 / (pw + after + rw) as f64).abs() < 1e-12);
            kinds[usize::from(!cont)] += 1;
        }
        assert!(kinds[0] > 0 && kinds[1] > 0);
        // 1:1 by key in every domain x generator cell, counting recorded failures
        let mut cell: BTreeMap<(String, String), [usize; 2]> = BTreeMap::new();
        for it in &p.items {
            cell.entry((it.domain.clone(), it.generator.clone())).or_default()[usize::from(it.kind == "splice")] += 1;
        }
        for f in &p.failures {
            let d = by[f["parent_id"].as_str().unwrap()].domain.to_string();
            cell.entry((d, f["generator"].as_str().unwrap().into())).or_default()[usize::from(f["kind"] == "splice")] += 1;
        }
        assert!(cell.values().all(|[c, s]| c.abs_diff(*s) <= 1), "{cell:?}");
        // ids disjoint from the instruct cohorts; deterministic
        let idf = fixture_idf(&hs);
        let v3 = build_v3(&V3Inputs { humans: &hs, idf: &idf, splits: &["train".to_string()], prefix_every: 1, middle_every: 1 }).unwrap();
        let ids: BTreeSet<&str> = v3.items.iter().map(|i| i.item_id.as_str()).chain(v3.requests.iter().map(|r| r.request_id.as_str())).collect();
        assert!(p.items.iter().all(|i| !ids.contains(i.item_id.as_str()) && !ids.contains(i.requests[0].as_str())));
        let q = build_v3_base(&hs, &gens).unwrap();
        assert_eq!((rows_sha(&p.items), rows_sha(&p.requests), rows_sha(&p.failures)), (rows_sha(&q.items), rows_sha(&q.requests), rows_sha(&q.failures)));
        // one family: every Train human goes to it, kinds still 1:1 per domain
        let one = build_v3_base(&hs, &["ernie_base"]).unwrap();
        assert!(one.items.iter().all(|i| i.generator == "ernie_base"));
        assert_eq!(one.items.len() + one.failures.len(), train.len());
        assert!(COHORTS.contains(&COHORT_BASE));
    }

    /// The base items follow the Tier C robustness rules 1:1: for the same
    /// passage and kind, spans, prompt, budget, target, endpoint, template and
    /// decoding equal those of `plan::build_test`'s OLMo-2 base slice.
    #[test]
    fn base_mode_equals_tier_c_robustness_rules() {
        let hs: Vec<Human> = fixture().into_iter().filter(|h| h.split == "train").collect();
        let test_hs: Vec<Human> = hs
            .iter()
            .map(|h| Human { split: "test".into(), id: format!("t-{}", h.id), title: h.title.clone(), text: h.text.clone(), text_sha256: h.text_sha256.clone(),
                word_count: h.word_count, source_group: h.source_group.clone(), genre: h.genre.clone(), domain: h.domain })
            .collect();
        let idf = fixture_idf(&hs);
        let t = plan::build_test(&test_hs, &idf).unwrap();
        let b = build_v3_base(&hs, &all_base()).unwrap();
        let treq: HashMap<&str, &Request> = t.requests.iter().map(|r| (r.item_id.as_str(), r)).collect();
        let mut matched = [0usize; 2];
        for (it, rq) in b.items.iter().zip(&b.requests) {
            let Some(ti) = t.items.iter().find(|x| x.cohort == plan::TEST_ROBUSTNESS_COHORT && x.parent_text_sha256 == it.parent_text_sha256 && x.kind == it.kind) else { continue };
            let tr = treq[ti.item_id.as_str()];
            assert_eq!((&it.source_spans, &it.template_id, &it.decoding, it.requested_fraction), (&ti.source_spans, &ti.template_id, &ti.decoding, ti.requested_fraction));
            assert_eq!((&rq.prompt, rq.cut_budget, rq.target_words, &rq.endpoint, rq.source_span), (&tr.prompt, tr.cut_budget, tr.target_words, &tr.endpoint, tr.source_span));
            matched[usize::from(it.kind == "splice")] += 1;
        }
        assert!(matched[0] > 0 && matched[1] > 0, "{matched:?}");
    }

    /// Reads one HTTP/1.1 request: request line and body.
    fn read_http(s: &mut std::net::TcpStream) -> Option<(String, Vec<u8>)> {
        use std::io::Read;
        let mut buf = Vec::new();
        let mut tmp = [0u8; 4096];
        let head_end = loop {
            let n = s.read(&mut tmp).ok()?;
            if n == 0 {
                return None;
            }
            buf.extend_from_slice(&tmp[..n]);
            if let Some(p) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                break p + 4;
            }
        };
        let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
        let len = head
            .lines()
            .find_map(|l| l.split_once(':').filter(|(k, _)| k.eq_ignore_ascii_case("content-length")).and_then(|(_, v)| v.trim().parse::<usize>().ok()))
            .unwrap_or(0);
        while buf.len() < head_end + len {
            let n = s.read(&mut tmp).ok()?;
            if n == 0 {
                break;
            }
            buf.extend_from_slice(&tmp[..n]);
        }
        Some((head.lines().next()?.to_owned(), buf[head_end..head_end + len].to_vec()))
    }

    /// Base items run on `/completion` with the raw prefix as the prompt (no
    /// chat messages), through the generate acceptance path, against a local
    /// mock server: no model is called. The accepted responses assemble with
    /// exact word labels. Two Tier C acceptance rules of the completion path
    /// reject: a reply that copies a 12-word window of the removed human text
    /// (a base model reciting the passage), and a continuation under 20 words.
    #[test]
    fn base_mode_requests_run_on_completion_endpoint() {
        use std::io::Write;
        let hs = fixture();
        let p = build_v3_base(&hs, &all_base()).unwrap();
        let of_kind = |kind: &str| -> Vec<Request> {
            let mut v: Vec<Request> = p.requests.iter().filter(|r| p.items.iter().any(|i| i.item_id == r.item_id && i.kind == kind)).cloned().collect();
            v.sort_by_key(|r| std::cmp::Reverse((r.target_words, r.request_id.clone())));
            v
        };
        let (conts, splices) = (of_kind("continuation"), of_kind("splice"));
        // accepted: the longest continuation and splice; copy: the next splice;
        // short: a continuation whose 19-word reply is inside the word window
        let short = conts.iter().skip(1).find(|r| (20..=38).contains(&r.target_words)).expect("fixture has a short continuation").clone();
        let reqs = [conts[0].clone(), splices[0].clone(), splices[1].clone(), short];
        assert!(reqs[..3].iter().all(|r| r.target_words >= 30), "fixture has long enough runs");
        assert!(reqs.iter().all(|r| r.endpoint == "completion" && r.split == "train" && r.op.is_none()));
        let by: HashMap<&str, &Human> = hs.iter().map(|h| (h.id.as_str(), h)).collect();
        // fresh-vocabulary sentences, disjoint per reply so no reply duplicates
        // another, longer than any budget so the cut applies
        let vocab = [
            ["novel", "signal", "copper", "lattice", "meridian", "canyon", "glacier", "prairie", "tundra", "sierra"],
            ["beacon", "cinder", "dune", "estuary", "falcon", "geyser", "hollow", "isthmus", "jetty", "knoll"],
            ["mosaic", "nimbus", "obsidian", "plume", "quill", "rampart", "sextant", "trellis", "upland", "vortex"],
            ["acorn", "bramble", "clover", "dew", "fern", "gorse", "heather", "ivy", "lichen", "moss"],
        ];
        let fresh = |f: &[&str; 10]| -> String {
            (0..40).map(|i| format!("Then {} {} {} {} met the {}.", f[i % 10], f[(i + 3) % 10], f[(i + 7) % 10], f[(i + 1) % 10], f[(i + 5) % 10])).collect::<Vec<_>>().join(" ")
        };
        // the copy reply recites the first 13 words of its removed span, then
        // goes on in fresh words; the short reply is one 19-word sentence
        let (cs, ce) = reqs[2].source_span.unwrap();
        let removed = &by[reqs[2].parent_id.as_str()].text[cs..ce];
        let recited: Vec<&str> = removed.split_whitespace().take(13).collect();
        let recited = recited.join(" ");
        let recited = if recited.ends_with('.') { recited } else { format!("{recited}.") };
        let short_reply = {
            let ws: Vec<&str> = (0..19).map(|i| vocab[3][i % 10]).collect();
            format!(" Then {}.", ws[1..].join(" "))
        };
        assert_eq!(text::words(&short_reply), 19);
        let replies =
            [format!(" {} And", fresh(&vocab[0])), format!(" {} And", fresh(&vocab[1])), format!(" {recited} {} And", fresh(&vocab[2])), short_reply];
        assert!(crate::overlap::shares_window(&replies[2], removed, COPY_WINDOW) && replies[2].trim() != removed.trim());
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        // the mock server answers by prompt, so a retried attempt gets the same reply
        let bodies: HashMap<String, Vec<u8>> = reqs
            .iter()
            .zip(&replies)
            .map(|(r, c)| (r.prompt.clone(), serde_json::to_vec(&json!({"content":c,"stop_type":"limit","timings":{}})).unwrap()))
            .collect();
        assert_eq!(bodies.len(), reqs.len());
        let server = std::thread::spawn(move || {
            let mut seen = Vec::new();
            for s in listener.incoming() {
                let mut s = s.unwrap();
                let Some((line, body)) = read_http(&mut s) else { continue };
                if line.starts_with("QUIT") {
                    break;
                }
                let b: Value = serde_json::from_slice(&body).unwrap();
                let out = &bodies[b["prompt"].as_str().unwrap()];
                let head = format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", out.len());
                s.write_all(head.as_bytes()).unwrap();
                s.write_all(out).unwrap();
                seen.push((line, b));
            }
            seen
        });
        let tok: tokenizers::Tokenizer = r#"{"version":"1.0","truncation":null,"padding":null,"added_tokens":[],"normalizer":null,"pre_tokenizer":{"type":"Whitespace"},"post_processor":null,"decoder":null,"model":{"type":"WordLevel","vocab":{"[UNK]":0},"unk_token":"[UNK]"}}"#
            .parse()
            .unwrap();
        let ctx = std::sync::Arc::new(generation::Ctx {
            endpoint: format!("http://{addr}"),
            alias: "base".into(),
            config: json!({}),
            tokenizer: tok,
            overlap: crate::overlap::Overlap::new(crate::overlap::PriorIndex::empty()),
            parents: hs.iter().map(|h| (h.id.clone(), h.text.clone())).collect(),
            client: generation::client().unwrap(),
            post: None,
            doc_words: None,
        });
        let dir = tempfile::tempdir().unwrap();
        let stats = generation::run(ctx, reqs.iter().map(|r| (r.clone(), 0)).collect(), dir.path(), 1).unwrap();
        std::net::TcpStream::connect(addr).unwrap().write_all(b"QUIT / HTTP/1.1\r\n\r\n").unwrap();
        let seen = server.join().unwrap();
        assert_eq!((stats["processed"].as_u64(), stats["accepted"].as_u64(), stats["failed"].as_u64()), (Some(4), Some(2), Some(2)), "{stats}");
        // two accepted on the first attempt, two rejected on both attempts
        let attempts = [&reqs[0], &reqs[1], &reqs[2], &reqs[2], &reqs[3], &reqs[3]];
        assert_eq!(seen.len(), attempts.len());
        for (i, ((line, body), r)) in seen.iter().zip(&attempts).enumerate() {
            assert_eq!(line, "POST /completion HTTP/1.1");
            assert_eq!(body["prompt"], json!(r.prompt));
            assert!(body.get("messages").is_none() && body.get("model").is_none());
            assert_eq!((body["n_predict"].as_u64(), body["temperature"].as_f64(), body["top_p"].as_f64()), (Some(1024), Some(0.8), Some(0.95)));
            let retry = u32::from(i > 0 && std::ptr::eq(attempts[i - 1], *r));
            assert_eq!(body["seed"].as_u64(), Some(r.seed.wrapping_add(retry) as u64));
        }
        let states = generation::load_states(&[dir.path().to_path_buf()]).unwrap();
        for (r, reply) in reqs[..2].iter().zip(&replies) {
            let v = &states[&r.request_id].accepted().unwrap()["verdict"];
            let clean = v["text"].as_str().unwrap();
            let w = text::words(clean);
            assert!(w <= r.target_words && w * 2 >= r.target_words && clean.ends_with('.') && reply.trim_start().starts_with(clean), "{v}");
            assert_eq!(v["diag"]["raw_words"].as_u64(), Some(text::words(reply) as u64));
            let h = by[r.parent_id.as_str()];
            let span = r.source_span.unwrap();
            let (fin, spans, _) = generation::assemble(&h.text, &[span], &[clean]).unwrap();
            let runs = label_runs(&word_labels(&fin, &spans).unwrap());
            assert_eq!(runs[1], (1, w));
            assert_eq!(runs[0], (0, text::words(&h.text[..span.0])));
        }
        // each rejection has exactly its own reason, on both attempts
        for (r, reason) in [(&reqs[2], "copies_removed_human_text_12_word_window"), (&reqs[3], "continuation_below_20_words")] {
            let st = &states[&r.request_id];
            assert!(st.accepted().is_none() && st.done());
            assert_eq!(st.responses.len(), generation::ATTEMPTS as usize);
            for v in st.responses.values() {
                assert_eq!(v["verdict"]["reasons"], json!([reason]), "{}", v["verdict"]);
            }
        }
        let short = &states[&reqs[3].request_id].responses[&0]["verdict"]["diag"];
        assert_eq!(short["words"].as_u64(), Some(19));
        // finalize: the accepted items become robustness Train-only records
        // with exact word label runs; the rejected ones are terminal failures
        let requests: HashMap<String, Request> = p.requests.iter().map(|r| (r.request_id.clone(), r.clone())).collect();
        let rows: HashMap<String, (String, Value)> = hs.iter().map(|h| (h.id.clone(), (h.text.clone(), json!({"collection":"synthetic"})))).collect();
        let tok2: tokenizers::Tokenizer = r#"{"version":"1.0","truncation":null,"padding":null,"added_tokens":[],"normalizer":null,"pre_tokenizer":{"type":"Whitespace"},"post_processor":null,"decoder":null,"model":{"type":"WordLevel","vocab":{"[UNK]":0},"unk_token":"[UNK]"}}"#
            .parse()
            .unwrap();
        let o = crate::finalize::finalize(&p.items, &requests, &states, &rows, &HashMap::new(), &p.failures, &tok2, "plan").unwrap();
        let recs = &o.records["train"];
        assert_eq!(recs.len(), 2);
        for r in &reqs[..2] {
            let rec = recs.iter().find(|x| x["id"] == r.item_id.as_str()).unwrap();
            assert_eq!((rec["cohort"].as_str(), rec["population"].as_str()), (Some(COHORT_BASE), Some("robustness_train_only")));
            let clean = states[&r.request_id].accepted().unwrap()["verdict"]["text"].as_str().unwrap().to_owned();
            let h = by[r.parent_id.as_str()];
            let (fin, spans, _) = generation::assemble(&h.text, &[r.source_span.unwrap()], &[clean.as_str()]).unwrap();
            assert_eq!(rec["text"].as_str(), Some(fin.as_str()));
            assert_eq!(rec["share_evidence"]["word_label_runs"], json!(label_runs(&word_labels(&fin, &spans).unwrap())));
        }
        let mut terminal: Vec<&str> = o.failures["train"].iter().filter(|f| f["status"] == "terminal_failure").map(|f| f["item_id"].as_str().unwrap()).collect();
        let mut want = [reqs[2].item_id.as_str(), reqs[3].item_id.as_str()];
        terminal.sort_unstable();
        want.sort_unstable();
        assert_eq!(terminal, want);
    }

    #[test]
    fn word_labels_exact_and_reject_cut_words() {
        let t = "alpha beta  gamma\ndelta";
        let s = t.find("beta").unwrap();
        let e = t.find("gamma").unwrap() + 5;
        assert_eq!(word_labels(t, &[(s, e)]).unwrap(), vec![0, 1, 1, 0]);
        assert_eq!(word_labels(t, &[(0, t.len())]).unwrap(), vec![1, 1, 1, 1]);
        assert!(word_labels(t, &[(2, e)]).is_err());
        assert_eq!(label_runs(&[0, 0, 1, 1, 1, 0]), vec![(0, 2), (1, 3), (0, 1)]);
    }
}
