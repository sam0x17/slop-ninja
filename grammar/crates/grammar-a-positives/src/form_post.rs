//! Profile v2 (`cc_cohort_profile_v2`, protocol A2 §6.2 as amended by
//! amendments 7 and 10) in the acceptance path of the local and spliced
//! positive generators, behind `--profile`.
//!
//! Every rule comes from `grammar-a-form`; this module only supplies the
//! generation context: the parent, the item, the population's prior, the
//! item's other accepted requests and the records.
//!
//! * **Profile off** (no `--profile`, or `--profile raw`): `Ctx::post` and
//!   `Ctx::doc_words` stay `None`, `generate` keeps `generation::verdict`
//!   and the spliced generator keeps `splice::verdict_fn`, so acceptance and
//!   journals are unchanged.
//! * **`--profile cc_cohort_profile_v2 --profile-population P`.** `FormPost`
//!   is the `generation::PostProcess`; it applies the layout transfer, the
//!   form and echo screens and the window `cc_final_length_window_v1`
//!   (150..=450 whitespace words) on the item's final document. Any reason
//!   rejects the attempt, which the request retries within its attempt
//!   limit. The returned text is the laid-out child; the cleaned response it
//!   read stays in the verdict as `raw_text` (and in the journal's
//!   `raw_body`). `Ctx::doc_words` stays `None`: the window belongs to this
//!   step because only the step knows which document is the final one.
//!   * **An item with one request** (a mirror, a single-span item or a
//!     single-edit spliced item). A mirror or a span goes through
//!     `grammar_a_form::profile::child_v2`, a spliced edit through
//!     `profile::item_v2` with that edit (insertions with their separator).
//!     The window reads the final document: the laid-out mirror, the parent
//!     with the laid-out span (`generation::assemble`), or the parent with
//!     the laid-out edit (`splice::assemble_parts`).
//!   * **An item with several requests** (a multi-span mixed item, or a
//!     spliced item with several edits). Its final document exists only
//!     when every request is accepted, so the item is judged as a whole when
//!     its last request is judged (`Completion`). That request's step is
//!     `profile::item_v2` over every edit's raw text: the form screen reads
//!     the assembled item's generated paragraphs (amendment 7 item 7.9), the
//!     echo screen compares every edit, and the window reads the final
//!     document, assembled as finalize assembles it (`generation::assemble`
//!     over the item's spans, or `splice::assemble_parts` over its edits). A
//!     rejection there is that request's attempt's, so it regenerates within
//!     its attempt limit (§6.2 "Failures regenerate"); accepted requests are
//!     never reopened, because the journal keeps an accepted response as
//!     final. Each earlier request gets the rules that read only its own
//!     edit: its layout transfer (line reasons and the empty check) and its
//!     echo comparison. The paragraph screen and the window read the final
//!     document, so they wait for the item; the partial document's paragraph
//!     reasons are recorded as `deferred_reasons` and reject nothing. The
//!     item's requests are judged one at a time (`Completion::gate`), so the
//!     last one always sees the others. Its siblings' raw texts come from
//!     earlier verdicts (`raw_text`), in this run or in an earlier one.
//!   * Every echo verdict comes from `grammar_a_form::echo::screen_item`,
//!     which both entry points call; this module has no echo comparison of
//!     its own.
//!   * `generate` runs its standard checks on the laid-out text after this
//!     step (`generation::verdict`). The spliced generator runs its own
//!     checks on the raw response first, because they read the response's
//!     own form (line breaks, markdown, quotes, punctuation), then this step
//!     (`spliced_verdict_fn`).
//! * **Conventions** are `profile::resolve_v2`: the parent's majorities,
//!   then keyed draws from the prior of the run population's parents in the
//!   parent's domain, then the population-wide prior, then none, under the
//!   population's own seed `norm_prior_seed(P)`. Draws are keyed by the item
//!   id: one item is one child document, so every request and attempt of an
//!   item sees the same choice.
//! * **The prior** holds the parents of the plan's items in the run's split
//!   (`generate`) or population (`slop_ninja_a_spliced`): the plan is the
//!   batch, and pilot and bulk runs of one plan compute the same prior. Its
//!   record (`prior_record`: ids, population, seed, the parents' digest and
//!   the tallies by domain; no text) is written once, before the batch's
//!   first request, as `form-prior-<sha256>.json` (mode 600, the exact bytes
//!   whose SHA-256 every post record, run record and binding names), and the
//!   dry run prints it. P must be on amendment 10 item 10.8's closed list
//!   (`grammar_a_form::profile::R2_POPULATIONS`).
//! * **The batch binding** (`check_binding`, `write_binding`): one batch's
//!   runs are judged under one profile record. A profile v2 run writes
//!   `form-profile-<batch>.json` (mode 600) before its first request; a later
//!   run with another record (profile, population, seed or prior) is
//!   refused, and so is a run with the profile off after a v2 run, or a v2
//!   run after requests were judged with the profile off.
//!
//! Records hold ids, counts, outcomes and digests, never text.
use crate::generation::{self, Ctx, PostOut, PostProcess, ReqState, VerdictFn, doc_window_reasons};
use crate::plan::Request;
use crate::splice;
use crate::text;
use anyhow::{Context, Result, anyhow, bail, ensure};
use grammar_a_form::conventions::{Conventions, Counts, KEY_SCOPE, counts};
use grammar_a_form::profile::{
    self, EditV2, ItemV2, NORM_ID, PROFILE_V2, PriorV2, Profile, is_r2_population, norm_prior_seed, resolve_v2,
};
use grammar_a_form::window::{LENGTH_WINDOW, LENGTH_WINDOW_ID};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// The schema of a prior record.
pub const PRIOR_SCHEMA: &str = "slop_ninja_a_positives_form_prior_v2";
/// The schema of a batch's profile binding.
pub const BINDING_SCHEMA: &str = "slop_ninja_a_positives_form_binding_v1";

/// The rule of an item with several requests, as the run record states it.
pub const ITEM_CHECK_RULE: &str = "an item with several spans or edits is judged as a whole when its last request is judged: item_v2 over every edit's raw text (form screen on the assembled item's generated paragraphs, echo on every edit) and the window on the final document as finalize assembles it; a rejection is that request's attempt's and regenerates within its attempt limit; each earlier request gets the rules that read only its own edit (its transfer's line reasons and empty check, its echo comparison), and its partial-document paragraph reasons are recorded as deferred_reasons";

/// The profile flags: `None` when the profile is off (no `--profile`, or
/// `raw`), else the population of a `cc_cohort_profile_v2` run. The v1
/// profiles are refused: §6.2 puts every round-2 positive under v2.
pub fn profile_population(profile: Option<&str>, population: Option<&str>) -> Result<Option<String>> {
    let p = match profile {
        None => None,
        Some(s) => Some(Profile::parse(s).map_err(|_| anyhow!("unknown profile {s} (known: {PROFILE_V2}, raw)"))?),
    };
    match (p, population) {
        (None | Some(Profile::Raw), None) => Ok(None),
        (None | Some(Profile::Raw), Some(_)) => bail!("--profile-population needs --profile {PROFILE_V2}"),
        (Some(Profile::CohortV2), Some(pop)) => {
            ensure!(is_r2_population(pop), "population {pop} is not a round-2 population id (protocol A2 amendment 10 item 10.8)");
            Ok(Some(pop.to_string()))
        }
        (Some(Profile::CohortV2), None) => bail!("--profile {PROFILE_V2} needs --profile-population (protocol A2 amendment 10 item 10.8)"),
        (Some(p), _) => bail!("profile {} is not wired into the A positive generators: protocol A2 §6.2 puts every positive under {PROFILE_V2}", p.id()),
    }
}

/// One request's splice: the parent byte range it replaces (`s == e` for an
/// insertion) and an insertion's separator.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Part {
    span: (usize, usize),
    insert_sep: Option<String>,
}

/// An item with several requests, in item order: (request id, parent span,
/// insertion separator). For a local mixed item the spans are its
/// `source_spans`; for a spliced item, its edits.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ItemSpec {
    pub item_id: String,
    pub parts: Vec<(String, (usize, usize), Option<String>)>,
}

#[derive(Clone, Debug)]
struct Accepted {
    /// The cleaned response the accepted verdict read (`raw_text`).
    raw: Option<String>,
    /// The accepted (laid-out) text.
    text: String,
}

/// The items with several requests and their accepted requests so far (see
/// the module notes). `gate` serialises the verdicts of these items'
/// requests, so the read of the siblings and the record of an acceptance
/// are one step.
#[derive(Debug, Default)]
pub struct Completion {
    /// Request id -> (item id, the request's index in its item).
    member: HashMap<String, (String, usize)>,
    /// Item id -> its requests and their parts, in item order.
    items: HashMap<String, Vec<(String, Part)>>,
    accepted: Mutex<HashMap<String, Accepted>>,
    gate: Mutex<()>,
}

impl Completion {
    /// Items and requests under the item check, and how many of the
    /// requests were already accepted when the run started.
    pub fn record(&self) -> Value {
        let acc = self.accepted.lock().unwrap_or_else(|e| e.into_inner());
        json!({"items": self.items.len(), "requests": self.member.len(), "accepted_at_start": acc.len()})
    }

    fn accept(&self, request_id: &str, v: &Value) {
        if let Some(t) = v["text"].as_str() {
            let a = Accepted { raw: v["raw_text"].as_str().map(String::from), text: t.to_string() };
            self.accepted.lock().unwrap_or_else(|e| e.into_inner()).insert(request_id.to_string(), a);
        }
    }
}

/// Profile v2's acceptance step for one population's run (see the module
/// notes).
#[derive(Clone, Debug)]
pub struct FormPost {
    population: String,
    seed: String,
    prior: PriorV2,
    /// Parent id -> the parent's domain (the prior's cell).
    domains: HashMap<String, String>,
    parents: usize,
    parents_sha256: String,
    prior_sha256: String,
    /// Request id -> spliced edit (the spliced generator's runs only).
    edits: HashMap<String, Part>,
    /// Final documents are assembled by the spliced generator's rules
    /// (`splice::assemble_parts`) rather than `generation::assemble`.
    spliced: bool,
    /// Whether the plan's items were bound (`with_items`); the wiring
    /// refuses a step without them.
    items_bound: bool,
    completion: Option<Arc<Completion>>,
}

impl FormPost {
    /// The step for `population` with the prior of its parents, as
    /// (id, domain, text). Every parent id must be distinct.
    pub fn new<'a>(population: &str, parents: impl IntoIterator<Item = (&'a str, &'a str, &'a str)>) -> Result<FormPost> {
        ensure!(is_r2_population(population), "population {population} is not a round-2 population id (protocol A2 amendment 10 item 10.8)");
        let mut domains: HashMap<String, String> = HashMap::new();
        let mut rows: Vec<(&str, &str, &str)> = Vec::new();
        for (id, domain, t) in parents {
            ensure!(domains.insert(id.to_string(), domain.to_string()).is_none(), "duplicate parent id {id}");
            rows.push((id, domain, t));
        }
        ensure!(!rows.is_empty(), "population {population} has no parent in this plan");
        let prior = PriorV2::of(population, rows.iter().map(|(_, d, t)| (*d, *t)));
        let mut listed: Vec<String> = rows.iter().map(|(id, d, t)| format!("{id}\t{d}\t{}\n", text::sha_hex(t.as_bytes()))).collect();
        listed.sort();
        let parents_sha256 = text::sha_hex(listed.concat().as_bytes());
        let mut fp = FormPost {
            population: population.to_string(),
            seed: norm_prior_seed(population),
            prior,
            domains,
            parents: rows.len(),
            parents_sha256,
            prior_sha256: String::new(),
            edits: HashMap::new(),
            spliced: false,
            items_bound: false,
            completion: None,
        };
        fp.prior_sha256 = text::sha_hex(&fp.prior_bytes());
        Ok(fp)
    }

    /// The step for a spliced run: each request id's edit (its parent span,
    /// `s == e` for an insertion, and an insertion's separator). Final
    /// documents are assembled by `splice::assemble_parts`.
    pub fn with_edits(mut self, edits: &HashMap<String, splice::Edit>) -> FormPost {
        self.edits = edits.iter().map(|(k, e)| (k.clone(), Part { span: e.parent_span, insert_sep: e.insert_sep.clone() })).collect();
        self.spliced = true;
        self
    }

    /// Binds the plan's items with several requests (`items`; items with one
    /// request may be passed and are ignored) and seeds their accepted
    /// requests from the journal `states` (each accepted verdict's `text`
    /// and `raw_text`). Required before the wiring.
    pub fn with_items(mut self, items: Vec<ItemSpec>, states: &HashMap<String, ReqState>) -> Result<FormPost> {
        let mut c = Completion::default();
        for it in items {
            if it.parts.len() < 2 {
                continue;
            }
            let mut parts = Vec::new();
            for (i, (rid, span, sep)) in it.parts.into_iter().enumerate() {
                ensure!(c.member.insert(rid.clone(), (it.item_id.clone(), i)).is_none(), "request {rid} is in two items");
                parts.push((rid, Part { span, insert_sep: sep }));
            }
            ensure!(c.items.insert(it.item_id.clone(), parts).is_none(), "duplicate item {}", it.item_id);
        }
        {
            let mut acc = c.accepted.lock().unwrap_or_else(|e| e.into_inner());
            for rid in c.member.keys() {
                if let Some(v) = states.get(rid).and_then(|s| s.accepted()) {
                    let t = v["verdict"]["text"].as_str().with_context(|| format!("accepted request {rid} has no text"))?;
                    acc.insert(rid.clone(), Accepted { raw: v["verdict"]["raw_text"].as_str().map(String::from), text: t.to_string() });
                }
            }
        }
        self.items_bound = true;
        self.completion = (!c.items.is_empty()).then(|| Arc::new(c));
        Ok(self)
    }

    pub fn population(&self) -> &str {
        &self.population
    }

    pub fn prior_sha256(&self) -> &str {
        &self.prior_sha256
    }

    /// The item check's counts (`Completion::record`), when items with
    /// several requests are bound.
    pub fn items_record(&self) -> Value {
        match &self.completion {
            Some(c) => c.record(),
            None => json!({"items": 0, "requests": 0, "accepted_at_start": 0}),
        }
    }

    /// The prior's record: no text. Its SHA-256 (of `prior_bytes`) is
    /// `prior_sha256`.
    pub fn prior_record(&self) -> Value {
        json!({"schema": PRIOR_SCHEMA, "profile": PROFILE_V2, "normaliser": NORM_ID, "population": self.population, "seed": self.seed,
            "key_scope": KEY_SCOPE, "parents": self.parents, "parents_sha256": self.parents_sha256, "prior": self.prior,
            "parents_by_domain": self.prior.by_domain.iter().map(|(d, p)| (d.clone(), p.parents)).collect::<BTreeMap<_, _>>(),
            "seed_rule": "norm_prior_seed: SHA-256 hex of 'slop_ninja_a_comp_r2_plan_v1:norm_prior:' + population (protocol A2 V21; amendment 7 item 7.8, amendment 10 item 10.8)",
            "rule": "the parents of the plan's items in the run's split or population; a child's keyed draw (KEY_SCOPE, this seed, the item id) uses its parent's domain cell when that cell holds a parent with the convention, else the population-wide prior, else none; quotes fall back to the parent's other quote class first"})
    }

    /// The bytes of the prior record that `prior_sha256` hashes and
    /// `write_binding` writes.
    pub fn prior_bytes(&self) -> Vec<u8> {
        serde_json::to_vec(&self.prior_record()).expect("prior record serialises")
    }

    /// The run record: the profile's ids, the population, the prior's
    /// digest and counts, and the acceptance rules this step adds. No text.
    /// It is also the batch binding's record.
    pub fn record(&self) -> Value {
        let mut r = Profile::CohortV2.record();
        r["population"] = json!(self.population);
        r["seed"] = json!(self.seed);
        r["prior_sha256"] = json!(self.prior_sha256);
        r["prior_parents"] = json!(self.parents);
        r["prior_parents_sha256"] = json!(self.parents_sha256);
        r["acceptance_window_document"] = json!("the item's final document: the laid-out mirror, the parent with the item's one laid-out span or edit, or the parent with all of an item's spans or edits when its last request is judged");
        r["item_check"] = json!(ITEM_CHECK_RULE);
        r
    }

    /// The item's conventions: the parent's counts, then the prior's keyed
    /// draws for document `doc` (the item id). None when the parent is not
    /// one of the prior's.
    pub fn conventions(&self, parent_id: &str, parent: &str, doc: &str) -> Option<(Conventions, &str, Counts)> {
        let domain = self.domains.get(parent_id)?;
        let c = counts(parent);
        Some((resolve_v2(&c, &self.prior, domain, &self.seed, doc), domain.as_str(), c))
    }

    fn base_record(&self, r: &Request) -> Value {
        json!({"profile": PROFILE_V2, "population": self.population, "prior_sha256": self.prior_sha256, "parent_id": r.parent_id, "doc_id": r.item_id})
    }

    /// The final document of an item from the parent and every request's
    /// laid-out text, as finalize assembles it.
    fn assemble(&self, parent: &str, parts: &[&Part], reps: &[&str]) -> Result<String, String> {
        if self.spliced {
            let ps: Vec<splice::Part> = parts.iter().map(|p| splice::Part { parent_span: p.span, insert_sep: p.insert_sep.as_deref() }).collect();
            splice::assemble_parts(parent, &ps, reps).map(|a| a.text).map_err(|e| e.to_string())
        } else {
            let spans: Vec<(usize, usize)> = parts.iter().map(|p| p.span).collect();
            generation::assemble(parent, &spans, reps).map(|x| x.0).map_err(|e| e.to_string())
        }
    }

    /// A request of an item with one request (see the module notes).
    fn single(&self, r: &Request, p: &str, conv: &Conventions, clean: &str) -> (String, Vec<String>, Value) {
        let (text, mut reasons, mut record, doc, kind) = match self.edits.get(&r.request_id) {
            Some(e) => {
                let it = profile::item_v2(p, conv, &[EditV2 { span: e.span, insert_sep: e.insert_sep.as_deref(), raw: clean }]);
                let text = it.reps.into_iter().next().unwrap_or_default();
                let doc = (!text.is_empty()).then(|| self.assemble(p, &[e], &[text.as_str()]));
                (text, it.reasons, it.record, doc, "the parent with the item's one edit")
            }
            None => {
                let ch = profile::child_v2(p, r.source_span, conv, clean);
                let doc = (!ch.text.is_empty()).then(|| match r.source_span {
                    None => Ok(ch.text.clone()),
                    Some(sp) => generation::assemble(p, &[sp], &[ch.text.as_str()]).map(|x| x.0).map_err(|e| e.to_string()),
                });
                let kind = if r.source_span.is_some() { "the parent with the item's one span" } else { "the laid-out mirror" };
                (ch.text, ch.reasons, ch.record, doc, kind)
            }
        };
        if let Some(d) = doc {
            window(d, kind, &mut reasons, &mut record);
        }
        (text, reasons, record)
    }

    /// A request of an item with several requests (see the module notes).
    fn of_item(&self, c: &Completion, item: &str, idx: usize, p: &str, conv: &Conventions, clean: &str) -> (String, Vec<String>, Value) {
        let parts = &c.items[item];
        let acc: HashMap<String, Accepted> = {
            let all = c.accepted.lock().unwrap_or_else(|e| e.into_inner());
            parts.iter().filter_map(|(rid, _)| all.get(rid).map(|a| (rid.clone(), a.clone()))).collect()
        };
        let pending: Vec<&str> = parts.iter().enumerate().filter(|(j, (rid, _))| *j != idx && !acc.contains_key(rid)).map(|(_, (rid, _))| rid.as_str()).collect();
        let missing: Vec<&str> =
            parts.iter().enumerate().filter(|(j, (rid, _))| *j != idx && acc.get(rid).is_some_and(|a| a.raw.is_none())).map(|(_, (rid, _))| rid.as_str()).collect();
        let own = &parts[idx].1;
        if !pending.is_empty() || !missing.is_empty() {
            let it = profile::item_v2(p, conv, &[EditV2 { span: own.span, insert_sep: own.insert_sep.as_deref(), raw: clean }]);
            let deferred = deferred_reasons(&it);
            let mut reasons: Vec<String> = it.reasons.iter().filter(|x| !deferred.contains(*x)).cloned().collect();
            if pending.is_empty() {
                // every sibling is accepted but one has no raw text: the item
                // cannot be judged as a whole, so it fails closed
                reasons.push("item_sibling_raw_missing".into());
            }
            let mut record = it.record;
            record["item_check"] = json!({"requests": parts.len(), "index": idx, "last": false, "pending": pending, "raw_missing": missing,
                "deferred_reasons": deferred.into_iter().collect::<Vec<_>>(), "rule": ITEM_CHECK_RULE});
            record["length_window"] = json!({"id": LENGTH_WINDOW_ID, "document": "deferred to the item's last request"});
            return (it.reps.into_iter().next().unwrap_or_default(), reasons, record);
        }
        let raws: Vec<&str> = parts.iter().enumerate().map(|(j, (rid, _))| if j == idx { clean } else { acc[rid].raw.as_deref().unwrap_or_default() }).collect();
        let edits: Vec<EditV2> =
            parts.iter().zip(&raws).map(|((_, part), raw)| EditV2 { span: part.span, insert_sep: part.insert_sep.as_deref(), raw }).collect();
        let it = profile::item_v2(p, conv, &edits);
        let mut reasons = it.reasons.clone();
        let mut siblings = Vec::new();
        for (j, (rid, _)) in parts.iter().enumerate() {
            if j == idx {
                continue;
            }
            let a = &acc[rid];
            let same = it.reps.get(j).is_some_and(|t| *t == a.text);
            if !same {
                reasons.push("item_sibling_text_mismatch".into());
            }
            siblings.push(json!({"request_id": rid, "raw_response_sha256": text::sha_hex(a.raw.as_deref().unwrap_or_default().as_bytes()),
                "accepted_text_sha256": text::sha_hex(a.text.as_bytes()), "text_is_item_v2s": same}));
        }
        let mut record = it.record.clone();
        let text = it.reps.get(idx).cloned().unwrap_or_default();
        if it.reps.iter().all(|t| !t.is_empty()) {
            let ps: Vec<&Part> = parts.iter().map(|(_, part)| part).collect();
            let reps: Vec<&str> = it.reps.iter().map(String::as_str).collect();
            let doc = self.assemble(p, &ps, &reps);
            if let (Ok(d), Some(a)) = (&doc, &it.assembled)
                && *d != a.text
            {
                reasons.push("item_assembly_mismatch".into());
            }
            window(doc, "the parent with all of the item's spans or edits (its final document)", &mut reasons, &mut record);
        }
        reasons.sort();
        reasons.dedup();
        record["item_check"] = json!({"requests": parts.len(), "index": idx, "last": true, "siblings": siblings, "rule": ITEM_CHECK_RULE});
        (text, reasons, record)
    }
}

/// The form reasons of an item_v2 verdict that read its assembled
/// document (the paragraph screen and assembly errors): the item's form
/// reasons less every edit's own (line reasons and the empty check).
fn deferred_reasons(it: &ItemV2) -> BTreeSet<String> {
    let strs = |v: &Value| -> BTreeSet<String> { v.as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default() };
    let mut own = BTreeSet::new();
    for e in it.record["edits"].as_array().into_iter().flatten() {
        own.extend(strs(&e["form_reasons"]));
    }
    strs(&it.record["form_reasons"]).difference(&own).cloned().collect()
}

/// The window on a final document (`kind` names it), or the reason its
/// assembly failed.
fn window(doc: Result<String, String>, kind: &str, reasons: &mut Vec<String>, record: &mut Value) {
    let (lo, hi) = LENGTH_WINDOW;
    match doc {
        Ok(d) => {
            let n = text::words(&d);
            record["length_window"] = json!({"id": LENGTH_WINDOW_ID, "min_words": lo, "max_words": hi, "document": kind, "words": n});
            reasons.extend(doc_window_reasons(n, LENGTH_WINDOW));
        }
        Err(e) => {
            record["length_window"] = json!({"id": LENGTH_WINDOW_ID, "document": kind, "assembly_error": e});
            reasons.push(format!("window_assembly_error:{e}"));
        }
    }
    reasons.sort();
    reasons.dedup();
}

impl PostProcess for FormPost {
    fn apply(&self, r: &Request, parent: Option<&str>, clean: &str) -> PostOut {
        let Some(p) = parent else {
            let mut record = self.base_record(r);
            record["form_reasons"] = json!(["form_parent_missing"]);
            return PostOut { text: clean.to_string(), reasons: vec!["form_parent_missing".into()], record };
        };
        let Some((conv, domain, c)) = self.conventions(&r.parent_id, p, &r.item_id) else {
            let mut record = self.base_record(r);
            record["form_reasons"] = json!(["form_parent_not_in_prior"]);
            return PostOut { text: clean.to_string(), reasons: vec!["form_parent_not_in_prior".into()], record };
        };
        let member = self.completion.as_ref().and_then(|cm| cm.member.get(&r.request_id).map(|m| (cm, m)));
        let (text, reasons, mut record) = match member {
            Some((cm, (item, idx))) => self.of_item(cm, item, *idx, p, &conv, clean),
            None => self.single(r, p, &conv, clean),
        };
        for (k, v) in self.base_record(r).as_object().expect("object") {
            record[k] = v.clone();
        }
        record["domain"] = json!(domain);
        record["parent_counts"] = json!(c);
        PostOut { text, reasons, record }
    }

    /// Always: `profile_population` admits no other profile, so a
    /// `FormPost` is profile v2's step and its verdicts keep `raw_text`.
    fn is_profile_v2(&self) -> bool {
        true
    }
}

/// The spliced generator's acceptance under the profile: `splice::verdict`
/// on the raw response (unchanged), then `Ctx::post` on the cleaned
/// response (the layout, the screens and the window). The verdict keeps the
/// laid-out text and the cleaned response (`raw_text`); every reason
/// rejects.
pub fn spliced_verdict_fn(edits: HashMap<String, splice::Edit>) -> Arc<VerdictFn> {
    Arc::new(move |ctx: &Ctx, r: &Request, status: u16, parsed: &Value| -> Value {
        match (edits.get(&r.request_id), ctx.parents.get(&r.parent_id)) {
            (Some(e), Some(p)) => {
                let mut v = splice::verdict(e, p, &ctx.overlap, status, parsed);
                spliced_post(ctx, r, p, &mut v);
                v
            }
            _ => json!({"accepted":false,"reasons":["edit_or_parent_missing"],"diag":{}}),
        }
    })
}

/// Applies `Ctx::post` to a spliced verdict that holds a cleaned response
/// (`text`); a verdict without one is left as it is. As in
/// `generation::verdict`, the cleaned response is kept as `raw_text` only
/// when the step is profile v2's (`PostProcess::is_profile_v2`).
pub fn spliced_post(ctx: &Ctx, r: &Request, parent: &str, v: &mut Value) {
    let (Some(post), Some(raw)) = (&ctx.post, v["text"].as_str().map(str::to_owned)) else {
        return;
    };
    let o = post.apply(r, Some(parent), &raw);
    let mut reasons: Vec<String> = v["reasons"].as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default();
    reasons.extend(o.reasons);
    reasons.sort();
    reasons.dedup();
    v["diag"]["post"] = o.record;
    v["text"] = json!(o.text);
    if post.is_profile_v2() {
        v["raw_text"] = json!(raw);
    }
    v["accepted"] = json!(reasons.is_empty());
    v["reasons"] = json!(reasons);
}

/// Wraps an acceptance function so that the requests of items with several
/// requests are judged one at a time and each acceptance is recorded for
/// the item's last request (`Completion`).
fn gated(c: Option<Arc<Completion>>, base: Arc<VerdictFn>) -> Arc<VerdictFn> {
    let Some(c) = c else { return base };
    Arc::new(move |ctx: &Ctx, r: &Request, status: u16, parsed: &Value| -> Value {
        if !c.member.contains_key(&r.request_id) {
            return base(ctx, r, status, parsed);
        }
        let _gate = c.gate.lock().unwrap_or_else(|e| e.into_inner());
        let v = base(ctx, r, status, parsed);
        if v["accepted"] == true {
            c.accept(&r.request_id, &v);
        }
        v
    })
}

/// A `Ctx`'s profile fields: (`post`, `doc_words`).
pub type CtxFields = (Option<Arc<dyn PostProcess>>, Option<(usize, usize)>);

/// `generate`'s `Ctx` fields and acceptance function under the profile
/// flags. Off: `None`, `None` and `generation::verdict` (what
/// `generation::run` uses). On: the step, no `doc_words` (the step applies
/// the window) and `generation::verdict` gated for items with several
/// requests. The step must have its items bound (`FormPost::with_items`).
pub fn generate_wiring(form: Option<FormPost>) -> Result<(CtxFields, Arc<VerdictFn>)> {
    let base: Arc<VerdictFn> = Arc::new(generation::verdict);
    match form {
        None => Ok(((None, None), base)),
        Some(f) => {
            ensure!(f.items_bound, "profile v2 needs the plan's items bound (FormPost::with_items), so an item with several requests is judged on its final document");
            let c = f.completion.clone();
            Ok(((Some(Arc::new(f) as Arc<dyn PostProcess>), None), gated(c, base)))
        }
    }
}

/// The spliced generator's `Ctx` fields and acceptance function under the
/// profile flags. Off: `None`, `None` and `splice::verdict_fn(edits)`, as
/// before the profile existed. On: the step (with `edits`), no `doc_words`
/// and `spliced_verdict_fn`, gated for items with several edits. The step
/// must have its items bound (`FormPost::with_items`).
pub fn spliced_wiring(form: Option<FormPost>, edits: HashMap<String, splice::Edit>) -> Result<(CtxFields, Arc<VerdictFn>)> {
    match form {
        None => Ok(((None, None), splice::verdict_fn(edits))),
        Some(f) => {
            ensure!(f.items_bound, "profile v2 needs the plan's items bound (FormPost::with_items), so an item with several edits is judged on its final document");
            let f = f.with_edits(&edits);
            let c = f.completion.clone();
            Ok(((Some(Arc::new(f) as Arc<dyn PostProcess>), None), gated(c, spliced_verdict_fn(edits))))
        }
    }
}

// ------------------------------------------------------------------ binding

/// The binding file of batch `batch` (for example `split-train` or
/// `population-train`) under a runs directory.
pub fn binding_path(runs: &Path, batch: &str) -> PathBuf {
    runs.join(format!("form-profile-{batch}.json"))
}

/// The prior record file of a prior under a runs directory: its exact bytes
/// hash to `prior_sha256`.
pub fn prior_path(runs: &Path, prior_sha256: &str) -> PathBuf {
    runs.join(format!("form-prior-{prior_sha256}.json"))
}

/// Refuses a run whose profile does not match its batch's binding at
/// `path`: `record` is the run's profile record (`FormPost::record`), or
/// `None` with the profile off; `earlier` says whether the batch already
/// has judged requests. A v2 run needs the same record as the binding, or
/// no binding and no earlier request; a run with the profile off needs no
/// binding.
pub fn check_binding(path: &Path, record: Option<&Value>, earlier: bool) -> Result<()> {
    if path.exists() {
        let b: Value = serde_json::from_slice(&fs::read(path)?).with_context(|| path.display().to_string())?;
        ensure!(b["schema"] == BINDING_SCHEMA, "{}: not a profile binding", path.display());
        let bound = &b["profile"];
        match record {
            None => bail!(
                "{}: this batch's runs are judged under {} (population {}); a run with the profile off would mix profiles in one batch",
                path.display(),
                bound["profile"],
                bound["population"]
            ),
            Some(r) => ensure!(
                bound == r,
                "{}: this batch's runs are judged under another profile record (population {}, seed {}, prior {}); this run's is population {}, seed {}, prior {}",
                path.display(),
                bound["population"],
                bound["seed"],
                bound["prior_sha256"],
                r["population"],
                r["seed"],
                r["prior_sha256"]
            ),
        }
    } else if record.is_some() && earlier {
        bail!("{}: this batch already has requests judged with the profile off; a profile v2 run would mix profiles in one batch", path.display());
    }
    Ok(())
}

/// Before a profile v2 run's first request: writes the prior record
/// (`prior_path`, its exact `prior_bytes`) and the batch binding at `path`
/// (mode 600), or checks that the files already there hold the same bytes.
pub fn write_binding(path: &Path, runs: &Path, f: &FormPost) -> Result<()> {
    fs::create_dir_all(runs)?;
    write_once(&prior_path(runs, &f.prior_sha256), &f.prior_bytes())?;
    let mut b = serde_json::to_vec_pretty(&json!({"schema": BINDING_SCHEMA, "profile": f.record()}))?;
    b.push(b'\n');
    write_once(path, &b)
}

/// Creates `path` with `bytes` (mode 600), or checks that it holds them.
pub fn write_once(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut o = fs::OpenOptions::new();
    o.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600);
    }
    match o.open(path) {
        Ok(mut f) => {
            f.write_all(bytes)?;
            f.sync_all()?;
            Ok(())
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            ensure!(fs::read(path)? == bytes, "{} exists with other bytes", path.display());
            Ok(())
        }
        Err(e) => Err(e).with_context(|| path.display().to_string()),
    }
}

/// Takes the exclusive lock of a batch's binding file for a run's duration
/// (released when the returned file drops). The spliced generator holds it,
/// because an item's edits can come from two generators' runs and the last
/// one must see the other's acceptances.
pub fn lock_binding(path: &Path) -> Result<fs::File> {
    let f = fs::File::open(path).with_context(|| path.display().to_string())?;
    match f.try_lock() {
        Ok(()) => Ok(f),
        Err(fs::TryLockError::WouldBlock) => {
            bail!("{}: another profile v2 run of this batch is running; an item's edits from two generators are judged in sequence", path.display())
        }
        Err(fs::TryLockError::Error(e)) => Err(e).with_context(|| path.display().to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plan;
    use grammar_a_form::profile::R2_POPULATIONS;

    /// Synthetic parents (no corpus text).
    const P1: &str = "The amber harbor kept its lantern lit through the night. Nobody on the basalt quay said \u{201C}cobalt tide\u{201D} aloud.\n\nBy morning the delta had turned to ember and garnet. The keepers walked the fjord road back to the meadow.";
    const P2: &str = "Juniper and kestrel stayed by the river - quiet for once. The orchid beds lay under a thin pebble crust.\n\nSaffron light reached the timber hall at noon. Indigo shadows moved slowly across the quartz floor.";

    fn req(id: &str, item: &str, parent: &str, span: Option<(usize, usize)>) -> Request {
        Request {
            request_id: id.into(),
            item_id: item.into(),
            parent_id: parent.into(),
            split: "train".into(),
            generator: "g".into(),
            endpoint: "chat".into(),
            template_id: "t".into(),
            prompt: id.into(),
            span_index: 0,
            source_span: span,
            target_words: 20,
            cut_budget: None,
            decoding: plan::profile("A", 0.7, 0.9, 1536),
            request_sha256: id.into(),
            seed: 1,
            op: None,
            cohort: None,
            attempt_limit: None,
        }
    }

    fn post(population: &str) -> FormPost {
        FormPost::new(population, [("p1", "fiction", P1), ("p2", "news", P2)]).unwrap()
    }

    #[test]
    fn profile_flags_select_v2_with_a_population_or_nothing() {
        assert_eq!(profile_population(None, None).unwrap(), None);
        assert_eq!(profile_population(Some("raw"), None).unwrap(), None);
        assert_eq!(profile_population(Some(PROFILE_V2), Some("span_select2")).unwrap().as_deref(), Some("span_select2"));
        for (p, pop, want) in [
            (Some("cc_cohort_profile_v1"), None, "is not wired"),
            (Some("cc_era_parent_norm_v1"), Some("train2"), "is not wired"),
            (Some("v2"), Some("train2"), "unknown profile"),
            (Some(PROFILE_V2), None, "needs --profile-population"),
            (None, Some("train2"), "needs --profile cc_cohort_profile_v2"),
            (Some("raw"), Some("train2"), "needs --profile cc_cohort_profile_v2"),
            (Some(PROFILE_V2), Some("train"), "not a round-2 population id"),
            (Some(PROFILE_V2), Some("Train2"), "not a round-2 population id"),
        ] {
            let e = profile_population(p, pop).unwrap_err().to_string();
            assert!(e.contains(want), "{p:?} {pop:?}: {e}");
        }
        // the closed list is grammar-a-form's, one source
        let mut ids = R2_POPULATIONS.to_vec();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), 14);
        for id in R2_POPULATIONS {
            assert_eq!(profile_population(Some(PROFILE_V2), Some(id)).unwrap().as_deref(), Some(id));
        }
    }

    #[test]
    fn the_prior_is_the_populations_own_and_order_free() {
        let a = post("train2");
        let b = FormPost::new("train2", [("p2", "news", P2), ("p1", "fiction", P1)]).unwrap();
        assert_eq!(a.prior_sha256(), b.prior_sha256());
        assert_eq!(a.prior_record(), b.prior_record());
        assert_eq!(text::sha_hex(&a.prior_bytes()), a.prior_sha256());
        let c = post("cal2_t");
        assert_ne!(a.prior_sha256(), c.prior_sha256());
        assert_eq!(c.prior_record()["seed"].as_str(), Some(norm_prior_seed("cal2_t").as_str()));
        assert_ne!(a.prior_record()["seed"], c.prior_record()["seed"]);
        let one = FormPost::new("train2", [("p1", "fiction", P1)]).unwrap();
        assert_ne!(a.prior_sha256(), one.prior_sha256());
        let other_domain = FormPost::new("train2", [("p1", "news", P1), ("p2", "news", P2)]).unwrap();
        assert_ne!(a.prior_sha256(), other_domain.prior_sha256());
        assert_eq!(a.prior_record()["parents_by_domain"], json!({"fiction": 1, "news": 1}));
        assert!(FormPost::new("train", [("p1", "fiction", P1)]).is_err());
        assert!(FormPost::new("train2", [("p1", "fiction", P1), ("p1", "news", P2)]).is_err());
        assert!(FormPost::new("train2", std::iter::empty()).is_err());
        for rec in [a.prior_record(), a.record()] {
            let s = rec.to_string();
            for t in [P1, P2] {
                for sent in text::sentences(t) {
                    assert!(!s.contains(&t[sent.start..sent.end]));
                }
            }
        }
        assert_eq!(a.record()["prior_sha256"].as_str(), Some(a.prior_sha256()));
        assert_eq!(a.record()["profile"], PROFILE_V2);
        assert_eq!(a.record()["item_check"], ITEM_CHECK_RULE);
    }

    #[test]
    fn apply_is_grammar_a_forms_child_step_keyed_by_the_item() {
        let fp = post("train2");
        let raw = "Lichen and fern covered the old marsh wall – nobody minded. \"Bramble\" was the only word they used...";
        // a mirror and a span, each equal to child_v2 with the item's conventions
        let s = text::sentences(P1)[1];
        for span in [None, Some((s.start, s.end))] {
            let r = req("r1", "item-1", "p1", span);
            let o = fp.apply(&r, Some(P1), raw);
            let (conv, domain, c) = fp.conventions("p1", P1, "item-1").unwrap();
            assert_eq!(domain, "fiction");
            let want = profile::child_v2(P1, span, &conv, raw);
            // P1 is short, so the window on the final document (the laid-out
            // mirror, or P1 with the laid-out span) rejects both
            let mut want_reasons = want.reasons.clone();
            want_reasons.push("final_document_below_150_words".into());
            want_reasons.sort();
            assert_eq!((o.text.as_str(), &o.reasons), (want.text.as_str(), &want_reasons));
            assert_eq!(o.record["conventions"], want.record["conventions"]);
            assert_eq!(o.record["echo"], want.record["echo"]);
            assert_eq!(o.record["population"], "train2");
            assert_eq!(o.record["prior_sha256"].as_str(), Some(fp.prior_sha256()));
            assert_eq!((o.record["doc_id"].as_str(), o.record["parent_id"].as_str(), o.record["domain"].as_str()), (Some("item-1"), Some("p1"), Some("fiction")));
            assert_eq!(o.record["parent_counts"], json!(c));
            let doc_words = match span {
                None => text::words(&want.text),
                Some(sp) => text::words(&generation::assemble(P1, &[sp], &[want.text.as_str()]).unwrap().0),
            };
            assert_eq!(o.record["length_window"]["words"].as_u64(), Some(doc_words as u64));
            // the parent's curly quotes are its own convention; the raw dash and dots are rendered too
            assert!(o.text.contains('\u{201C}') && !o.text.contains('"'), "{}", o.text);
        }
        // every request and attempt of one item sees the same conventions; the key is the item
        let a = fp.conventions("p2", P2, "item-7").unwrap().0;
        let b = fp.conventions("p2", P2, "item-7").unwrap().0;
        assert_eq!(serde_json::to_value(&a).unwrap(), serde_json::to_value(&b).unwrap());
        // an echo is rejected by the echo screen (raw and rendered), a parent outside the prior and a missing parent fail closed
        let echo = fp.apply(&req("r2", "item-2", "p1", None), Some(P1), P1);
        assert!(echo.reasons.contains(&"echo_mirror_byte_identical".to_string()) && echo.reasons.contains(&"echo_mirror_rendered_byte_identical".to_string()));
        let out = fp.apply(&req("r3", "item-3", "p9", None), Some(P1), raw);
        assert_eq!((out.text.as_str(), out.reasons.clone()), (raw, vec!["form_parent_not_in_prior".to_string()]));
        let gone = fp.apply(&req("r4", "item-4", "p1", None), None, raw);
        assert_eq!(gone.reasons, vec!["form_parent_missing".to_string()]);
    }

    /// A prior over curly, ASCII and quote-less parents of one domain, and a
    /// quote-less parent whose double-quote convention is a keyed draw.
    fn keyed_fixture(population: &str) -> (FormPost, &'static str) {
        let curly = "The amber harbor said \u{201C}cobalt\u{201D} twice that night. The basalt quay stayed quiet until the delta turned.";
        let ascii = "The garnet meadow said \"juniper\" once that morning. The kestrel lantern stayed lit until the nectar ran out.";
        let plain = "The orchid pebble stayed by the quartz river all day. The saffron timber hall stood empty until dusk came.";
        (FormPost::new(population, [("pa", "fiction", curly), ("pb", "fiction", ascii), ("pc", "fiction", plain)]).unwrap(), plain)
    }

    #[test]
    fn keyed_draws_take_the_item_id_not_the_request_id() {
        let (fp, plain) = keyed_fixture("train2");
        let quote = |k: &str| serde_json::to_value(fp.conventions("pc", plain, k).unwrap().0).unwrap()["double"].clone();
        let keys: Vec<String> = (0..64).map(|i| format!("k{i}")).collect();
        let (a, b) = keys
            .iter()
            .flat_map(|a| keys.iter().map(move |b| (a, b)))
            .find(|(a, b)| quote(a) != quote(b))
            .expect("two keys with different draws");
        assert_eq!(quote(a)["source"], "keyed_prior_domain");
        let o = fp.apply(&req(a, b, "pc", None), Some(plain), "Lichen and fern covered the marsh wall. \"Bramble\" was all they said at the ridge.");
        assert_eq!(o.record["conventions"]["double"], quote(b));
        assert_ne!(o.record["conventions"]["double"], quote(a));
        assert_eq!(o.record["doc_id"].as_str(), Some(b.as_str()));
    }

    #[test]
    fn keyed_draws_use_the_run_populations_seed() {
        // under cal2_t, the draw is cal2_t's seed's, and for some item it
        // differs from the draw train2's seed would give
        let (fp, plain) = keyed_fixture("cal2_t");
        let draw = |pop: &str, k: &str| {
            let c = counts(plain);
            serde_json::to_value(resolve_v2(&c, &fp.prior, "fiction", &norm_prior_seed(pop), k)).unwrap()["double"].clone()
        };
        let k = (0..256).map(|i| format!("item-{i}")).find(|k| draw("cal2_t", k) != draw("train2", k)).expect("an item whose draws differ by seed");
        let o = fp.apply(&req("r1", &k, "pc", None), Some(plain), "Lichen and fern covered the marsh wall. \"Bramble\" was all they said at the ridge.");
        assert_eq!(o.record["conventions"]["double"], draw("cal2_t", &k));
        assert_ne!(o.record["conventions"]["double"], draw("train2", &k));
        assert_eq!(o.record["population"], "cal2_t");
    }

    /// A post step that is not profile v2's (B1c).
    struct NotV2;

    impl PostProcess for NotV2 {
        fn apply(&self, _r: &Request, _parent: Option<&str>, clean: &str) -> PostOut {
            PostOut { text: clean.to_uppercase(), reasons: vec!["synthetic_reason".into()], record: json!({"step": "not_v2"}) }
        }
        fn is_profile_v2(&self) -> bool {
            false
        }
    }

    fn ctx_with(post: Option<Arc<dyn PostProcess>>) -> Ctx {
        Ctx {
            endpoint: String::new(),
            alias: "m".into(),
            config: json!({}),
            tokenizer: r#"{"version":"1.0","truncation":null,"padding":null,"added_tokens":[],"normalizer":null,"pre_tokenizer":{"type":"Whitespace"},"post_processor":null,"decoder":null,"model":{"type":"WordLevel","vocab":{"[UNK]":0},"unk_token":"[UNK]"}}"#
                .parse()
                .unwrap(),
            overlap: crate::overlap::Overlap::new(crate::overlap::PriorIndex::empty()),
            parents: [("p1".to_string(), P1.to_string()), ("p2".to_string(), P2.to_string())].into(),
            client: generation::client().unwrap(),
            post,
            doc_words: None,
        }
    }

    #[test]
    fn spliced_post_keeps_raw_text_only_under_profile_v2() {
        let fp = post("span_select2");
        assert!(fp.is_profile_v2());
        assert!(!NotV2.is_profile_v2());
        let s = text::sentences(P1)[1];
        let r = req("e1", "item-e1", "p1", Some((s.start, s.end)));
        let raw = "Lichen and fern covered the old marsh wall \u{2013} nobody minded at all.";
        let orig = json!({"accepted": true, "reasons": [], "text": raw, "diag": {"splice": "synthetic"}});
        // profile v2's step: its text and record, and the cleaned response as raw_text
        let mut v2 = orig.clone();
        spliced_post(&ctx_with(Some(Arc::new(fp.clone()))), &r, P1, &mut v2);
        let o = fp.apply(&r, Some(P1), raw);
        assert_eq!(v2["raw_text"].as_str(), Some(raw));
        assert_eq!(v2["text"].as_str(), Some(o.text.as_str()));
        assert_eq!(v2["diag"]["post"], o.record);
        // another step: its text, record and reasons merged, no raw_text
        let mut v1 = orig.clone();
        spliced_post(&ctx_with(Some(Arc::new(NotV2))), &r, P1, &mut v1);
        assert!(v1.get("raw_text").is_none(), "{v1}");
        assert_eq!(v1["text"].as_str(), Some(raw.to_uppercase().as_str()));
        assert_eq!(v1["diag"]["post"], json!({"step": "not_v2"}));
        assert_eq!((v1["accepted"].clone(), v1["reasons"].clone()), (json!(false), json!(["synthetic_reason"])));
        assert_eq!(v1["diag"]["splice"], "synthetic");
        // no step, or a verdict without a cleaned response: unchanged
        let mut off = orig.clone();
        spliced_post(&ctx_with(None), &r, P1, &mut off);
        assert_eq!(off, orig);
        let bare = json!({"accepted": false, "reasons": ["no_content"], "diag": {}});
        let mut b = bare.clone();
        spliced_post(&ctx_with(Some(Arc::new(fp))), &r, P1, &mut b);
        assert_eq!(b, bare);
    }

    #[test]
    fn the_wiring_needs_the_items_bound() {
        let e = generate_wiring(Some(post("train2"))).err().unwrap().to_string();
        assert!(e.contains("with_items"), "{e}");
        let e = spliced_wiring(Some(post("train2")), HashMap::new()).err().unwrap().to_string();
        assert!(e.contains("with_items"), "{e}");
        let fp = post("train2").with_items(Vec::new(), &HashMap::new()).unwrap();
        assert!(fp.completion.is_none());
        let ((p, w), _) = generate_wiring(Some(fp)).unwrap();
        assert!(p.is_some() && w.is_none());
        // a request in two items is refused
        let it = |id: &str| ItemSpec { item_id: id.into(), parts: vec![("r1".into(), (0, 3), None), (format!("r-{id}"), (4, 7), None)] };
        assert!(post("train2").with_items(vec![it("a"), it("b")], &HashMap::new()).is_err());
    }

    #[test]
    fn the_binding_refuses_mixed_profiles_in_one_batch() {
        let dir = tempfile::tempdir().unwrap();
        let runs = dir.path();
        let path = binding_path(runs, "split-train");
        let a = post("train2");
        let b = post("cal2_t");
        // nothing yet: off and v2 pass; v2 after off requests is refused
        check_binding(&path, None, true).unwrap();
        check_binding(&path, Some(&a.record()), false).unwrap();
        assert!(check_binding(&path, Some(&a.record()), true).unwrap_err().to_string().contains("profile off"));
        // the first v2 run writes the prior and the binding, mode 600
        write_binding(&path, runs, &a).unwrap();
        let pp = prior_path(runs, a.prior_sha256());
        assert_eq!(text::sha_hex(&fs::read(&pp).unwrap()), a.prior_sha256());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            for p in [&path, &pp] {
                assert_eq!(fs::metadata(p).unwrap().permissions().mode() & 0o777, 0o600);
            }
        }
        // the same record passes, writes again idempotently; another record or the profile off is refused
        check_binding(&path, Some(&a.record()), true).unwrap();
        write_binding(&path, runs, &a).unwrap();
        assert!(check_binding(&path, Some(&b.record()), true).unwrap_err().to_string().contains("another profile record"));
        assert!(check_binding(&path, None, true).unwrap_err().to_string().contains("profile off"));
        assert!(write_binding(&path, runs, &b).is_err());
        // the lock: a second holder is refused while the first holds it
        let held = lock_binding(&path).unwrap();
        assert!(lock_binding(&path).unwrap_err().to_string().contains("another profile v2 run"));
        drop(held);
        lock_binding(&path).unwrap();
    }
}
