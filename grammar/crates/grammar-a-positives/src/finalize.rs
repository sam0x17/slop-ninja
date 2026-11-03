//! Deterministic assembly of accepted responses into positive records, with
//! retained failures, a global generated-text dedup in plan order, the
//! no-op splice control and per-split accounting.
//!
//! **Raw twins and the screen-fail stratum** (protocol A2 §7.2). Both are
//! emitted whenever the journal's verdicts carry profile v2's raw-response
//! digest (the verdict's `diag.post` record names `raw_response_sha256`, or
//! the request's own edit digest under an item check; `raw_digest`), so no
//! flag is needed. With the profile off no verdict carries one and every
//! output is unchanged.
//! * **Raw twin (R-form).** An accepted record keeps its twin under
//!   `raw_twin`: the same accepted responses spliced raw into the same
//!   parent (`raw_twin`, `rebuild`). Each request's raw child is its
//!   accepted verdict's `raw_text`, read exactly (never trimmed; a mirror's
//!   raw child is the whole twin) and refused unless its SHA-256 equals the
//!   verdict's raw-response digest (`raw_child`); the parent must match
//!   `parent_text_sha256`, and an item check's record must name the same raw
//!   digests. The twin is a whole record: the N-form record's labels
//!   (origin, cohort, kind, population, generation, rights and the rest),
//!   with the text, spans, realised share, word counts, tokens and word
//!   label runs measured on its own document. Its id is `rform:<id>`,
//!   `twin_of` names the N-form record (the form-sensitivity pair,
//!   `fs_pairs`), and `rform` holds the raw digests and the rule. Its window
//!   verdict is the N-form document's (§7.2): it is emitted exactly when the
//!   N-form record is, and never window-checked again.
//! * **`rform_screen_fail_v1`** (§7.2, read by amendment 19 items
//!   19.1, 19.2 and 19.4).
//!   - An attempt is a *raw response* when its journal response line shows
//!     model content (`is_raw_response`): its verdict carries none of
//!     `transport_error`, `http_status_<code>` or `no_content`. An intent
//!     with no response line is not one. A request's first raw response is
//!     its lowest such attempt (`first_raw_response`), accepted or not: a
//!     response with content that was rejected before the profile step (a
//!     nonempty or unclosed reasoning block, no sentence end within a
//!     completion's budget) or with empty content is one, and its rejection
//!     is not the form screen's.
//!   - An item takes each request's first raw response. When one of them was
//!     rejected and every rejected one failed the form screen only
//!     (`failed_form_screen_only`: every reason is one of the post record's
//!     `form_reasons` and a screen code, `is_screen_reason`), the item gets
//!     one document: the parent with those raw responses spliced in,
//!     window-checked here (`cc_final_length_window_v1`). A request with no
//!     raw response, or a first raw response rejected by anything else
//!     (before the profile step, by the window, the echo screen, an item
//!     check or a standard check), keeps the item out.
//!   - A window or assembly failure leaves the document out, recorded without
//!     text. The document, or the note that it was left out, is held under
//!     `rform_screen_fail_v1` in the entry the item lands in (a record,
//!     failure or exclusion), with id `rform_screen_fail_v1:<item id>` and no
//!     `twin_of`: it has no N-form twin, so `fs_pairs` never pairs it.
//!
//! **Scope** (amendment 19 item 19.5). Both are emitted on every
//! split and counted there. The stratum enters only the R-form evaluation
//! populations (Dev-iter-2, Dev-select-2 and Test-2). N0's and S0-13's
//! positives are raw twins only (§8, item 11.5): a consumer takes
//! `raw_twin_doc`, never `screen_fail_doc`, for them.
//!
//! **Uniqueness** (item 19.6). The R-form documents of one run, twins
//! and stratum documents, are unique by text across every split: one whose
//! text an earlier one holds, in plan order, refuses the run.
//!
//! Records hold text; the left-out notes and every rule string hold none.
use crate::generation::{self, ReqState, doc_window_reasons};
use crate::overlap;
use crate::plan::{Item, Request};
use crate::text;
use anyhow::{Context, Result, ensure};
use grammar_a_form::window::{LENGTH_WINDOW, LENGTH_WINDOW_ID};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use tokenizers::Tokenizer;

pub const RECORD_SCHEMA: &str = "slop_ninja_a_comp_positive_v1";

/// The N-form record's field holding its raw twin.
pub const RAW_TWIN_FIELD: &str = "raw_twin";
/// The stratum of a raw twin (the `stratum` field of an R-form record).
pub const RAW_TWIN_STRATUM: &str = "raw_twin";
/// The screen-fail stratum (protocol A2 §7.2): its id, its R-form records'
/// `stratum`, the entry field holding its document and its id prefix.
pub const SCREEN_FAIL_ID: &str = "rform_screen_fail_v1";
/// The `form` of every R-form record.
pub const R_FORM: &str = "r_form";
/// A raw twin's id is this prefix and the N-form record's id.
pub const RAW_TWIN_ID_PREFIX: &str = "rform:";
/// The form screen's reason codes, by prefix (amendment 19 item
/// 19.4): the transfer's line screen (`form_line_`), the paragraph and
/// licence screens (`form_paragraph_`) and the frame screen (`form_frame_`).
/// Every other form code is outside the screen, a code added later included.
pub const SCREEN_REASON_PREFIXES: [&str; 3] = ["form_line_", "form_paragraph_", "form_frame_"];
/// The form codes outside the screen that `grammar-a-form` and the profile
/// wiring emit (item 19.4; a code ending in `:` is a prefix). They name
/// no screen: `is_screen_reason` reads `SCREEN_REASON_PREFIXES` only. Tests
/// check that every form code in those sources is in one list or the other.
pub const OUTSIDE_SCREEN_FORM_REASONS: [&str; 6] =
    ["form_empty", "form_span_outside_parent", "form_donor_is_a_span", "form_assembly_error:", "form_parent_missing", "form_parent_not_in_prior"];
/// Verdict reasons that mean the server returned no model content, so the
/// attempt is not a raw response (amendment 19 item 19.1); a code
/// ending in `_` is a prefix (`http_status_<code>`).
pub const NO_CONTENT_REASONS: [&str; 3] = ["transport_error", "http_status_", "no_content"];

/// The raw twin's rule, as its record states it.
pub const RAW_TWIN_RULE: &str = "protocol A2 section 7.2 raw twin: the item's accepted responses spliced raw into the same parent; each raw child is the accepted verdict's raw_text, read exactly and refused unless its SHA-256 is the verdict's profile v2 raw_response_sha256; the parent must match parent_text_sha256; a mirror is its raw child, spans by generation::assemble; the N-form record's labels, with text, spans, share, words, tokens and word label runs measured on this document; the window verdict is the N-form document's; the R-form documents of one run are unique by text, a duplicate refusing the run (amendment 19 item 19.6)";
/// The screen-fail stratum's rule, as its record states it.
pub const SCREEN_FAIL_RULE: &str = "protocol A2 section 7.2 rform_screen_fail_v1 as read by amendment 19 items 19.1, 19.2 and 19.4: each request's first raw response is its lowest attempt whose response verdict carries none of transport_error, http_status_<code> or no_content, accepted or not (a response rejected before the profile step, or with empty content, is one; an intent with no response is not); an item whose first raw responses include a rejection, every rejected one failing the form screen only (each reason one of the post record's form_reasons and a form_line_, form_paragraph_ or form_frame_ code), gets one document, those raw responses spliced into the parent by generation::assemble (a mirror is its raw response), window-checked at assembly (cc_final_length_window_v1); a request with no raw response, or any other rejection, keeps the item out; a window or assembly failure leaves the document out; no N-form twin; the R-form documents of one run are unique by text, a duplicate refusing the run (item 19.6)";

pub fn share_bin(s: f64) -> &'static str {
    if s <= 0.0 {
        "0"
    } else if s < 0.10 {
        "(0,10)"
    } else if s < 0.25 {
        "[10,25)"
    } else if s < 0.50 {
        "[25,50)"
    } else if s < 0.75 {
        "[50,75)"
    } else if s < 0.90 {
        "[75,90)"
    } else if s < 1.0 {
        "[90,100)"
    } else {
        "100"
    }
}

pub struct Out {
    pub records: BTreeMap<String, Vec<Value>>,
    pub failures: BTreeMap<String, Vec<Value>>,
    pub exclusions: BTreeMap<String, Vec<Value>>,
    pub summary: Value,
}

// ------------------------------------------------------------------ R-form

/// The profile v2 raw-response digest of a verdict's own raw child, from its
/// `diag.post` record: `raw_response_sha256` (a mirror or one span), or under
/// an item check the request's own edit (`edits[item_check.index]` when the
/// request was judged last, else its one edit), or a spliced item's one
/// edit. None for a record without one (the profile off, v1, or a step that
/// read no raw child).
pub fn raw_digest(post: &Value) -> Option<&str> {
    if let Some(d) = post["raw_response_sha256"].as_str() {
        return Some(d);
    }
    let edits = post["edits"].as_array()?;
    let k = if post["item_check"]["last"] == true {
        usize::try_from(post["item_check"]["index"].as_u64()?).ok()?
    } else if edits.len() == 1 {
        0
    } else {
        return None;
    };
    edits.get(k)?["raw_response_sha256"].as_str()
}

/// The raw child a verdict read (`raw_text`, exactly), checked against its
/// profile v2 raw-response digest. `Ok(None)` when the verdict carries no
/// digest; an error when it carries one but no raw text, or a raw text with
/// another SHA-256.
pub fn raw_child(verdict: &Value) -> Result<Option<&str>> {
    let Some(d) = raw_digest(&verdict["diag"]["post"]) else {
        return Ok(None);
    };
    let raw = verdict["raw_text"].as_str().context("the verdict carries a profile v2 raw_response_sha256 but no raw_text")?;
    ensure!(text::sha_hex(raw.as_bytes()) == d, "raw_text does not match the verdict's raw_response_sha256 {d}");
    Ok(Some(raw))
}

/// Whether a form reason is the form screen's: it starts with one of
/// `SCREEN_REASON_PREFIXES`. An unknown form code is outside the screen.
pub fn is_screen_reason(r: &str) -> bool {
    SCREEN_REASON_PREFIXES.iter().any(|p| r.starts_with(p))
}

/// Whether a verdict was rejected by the form screen only: it has reasons,
/// and each is one of its post record's `form_reasons` and a screen reason
/// (`is_screen_reason`). A window, echo, item-check or standard reason, or a
/// verdict with no post record, gives false.
pub fn failed_form_screen_only(verdict: &Value) -> bool {
    if verdict["accepted"] == true {
        return false;
    }
    let form: HashSet<&str> = verdict["diag"]["post"]["form_reasons"].as_array().map(|a| a.iter().filter_map(Value::as_str).collect()).unwrap_or_default();
    match verdict["reasons"].as_array() {
        Some(rs) if !rs.is_empty() => rs.iter().all(|r| r.as_str().is_some_and(|r| form.contains(r) && is_screen_reason(r))),
        _ => false,
    }
}

/// Whether a journal response line is a raw response (amendment 19 item
/// 19.1): its verdict carries none of `NO_CONTENT_REASONS`, so the
/// server returned model content. A response with content rejected before
/// the profile step (no post record, no digest), or with empty content, is
/// one.
pub fn is_raw_response(line: &Value) -> bool {
    let no_content = |r: &str| NO_CONTENT_REASONS.iter().any(|c| if c.ends_with('_') { r.starts_with(c) } else { r == *c });
    !line["verdict"]["reasons"].as_array().into_iter().flatten().filter_map(Value::as_str).any(no_content)
}

/// A request's first raw response: the journal response line of its lowest
/// attempt that is a raw response (`is_raw_response`), accepted or not, with
/// or without a profile v2 raw-response digest. An intent with no response
/// line (an uncertain attempt) is not in `responses`, so it is never one.
pub fn first_raw_response(st: &ReqState) -> Option<(u32, &Value)> {
    st.responses.iter().find(|(_, v)| is_raw_response(v)).map(|(a, v)| (*a, v))
}

/// A spliced document: (text, final byte spans, realised final-word share).
pub type Spliced = (String, Vec<(usize, usize)>, f64);

/// Splices raw children into the parent: a mirror is its one raw child
/// (exactly), anything else `generation::assemble` at the item's spans.
pub fn rebuild(parent: &str, kind: &str, spans: &[(usize, usize)], raws: &[&str]) -> Result<Spliced> {
    if kind == "mirror" {
        ensure!(spans.is_empty() && raws.len() == 1, "a mirror has no span and one raw child");
        ensure!(!raws[0].trim().is_empty(), "empty raw mirror");
        return Ok((raws[0].to_owned(), vec![(0, raws[0].len())], 1.0));
    }
    generation::assemble(parent, spans, raws)
}

/// One raw child an R-form document splices: its request, attempt and
/// digest, and its verdict's outcome.
#[derive(Clone, Debug)]
pub struct RawResponse {
    pub request_id: String,
    pub attempt: u32,
    pub raw_response_sha256: String,
    pub accepted: bool,
    pub reasons: Value,
}

impl RawResponse {
    fn of(request_id: &str, attempt: u32, line: &Value) -> RawResponse {
        let v = &line["verdict"];
        RawResponse {
            request_id: request_id.to_string(),
            attempt,
            raw_response_sha256: raw_digest(&v["diag"]["post"]).unwrap_or_default().to_string(),
            accepted: v["accepted"] == true,
            reasons: v["reasons"].clone(),
        }
    }

    fn record(&self) -> Value {
        json!({"request_id": self.request_id, "attempt": self.attempt, "raw_response_sha256": self.raw_response_sha256,
            "accepted": self.accepted, "reasons": self.reasons})
    }
}

/// An R-form document rebuilt from the parent and raw children.
#[derive(Clone, Debug)]
pub struct RawDoc {
    pub text: String,
    pub ai_spans: Vec<(usize, usize)>,
    pub share: f64,
    /// The raw children, in request order.
    pub raws: Vec<String>,
    pub responses: Vec<RawResponse>,
}

fn check_parent(it: &Item, ptext: &str) -> Result<()> {
    ensure!(text::sha_hex(ptext.as_bytes()) == it.parent_text_sha256, "{}: the parent text does not match parent_text_sha256", it.item_id);
    Ok(())
}

/// The raw twin of an accepted item, from its requests' accepted verdicts
/// (see the module notes). `Ok(None)` when no accepted verdict carries a
/// profile v2 raw-response digest; an error when some do and some do not, a
/// raw text fails its digest, the parent fails its digest, an item check
/// read other raw children, or the splice fails.
pub fn raw_twin(it: &Item, ptext: &str, states: &HashMap<String, ReqState>) -> Result<Option<RawDoc>> {
    let mut found: Vec<(&str, u32, &Value, Option<&str>)> = Vec::new();
    for rid in &it.requests {
        let st = states.get(rid).with_context(|| format!("{}: request {rid} has no journal", it.item_id))?;
        let (a, line) = st.responses.iter().find(|(_, v)| v["verdict"]["accepted"] == true).with_context(|| format!("{}: request {rid} has no accepted response", it.item_id))?;
        let raw = raw_child(&line["verdict"]).with_context(|| format!("{}: request {rid} attempt {a}", it.item_id))?;
        found.push((rid, *a, line, raw));
    }
    let with = found.iter().filter(|f| f.3.is_some()).count();
    if with == 0 {
        return Ok(None);
    }
    ensure!(with == found.len(), "{}: {with} of {} accepted requests carry a profile v2 raw_response_sha256", it.item_id, found.len());
    check_parent(it, ptext)?;
    let responses: Vec<RawResponse> = found.iter().map(|(rid, a, line, _)| RawResponse::of(rid, *a, line)).collect();
    for (rid, _, line, _) in &found {
        let post = &line["verdict"]["diag"]["post"];
        if post["item_check"]["last"] == true {
            let edits = post["edits"].as_array().with_context(|| format!("{}: request {rid}'s item check has no edits", it.item_id))?;
            ensure!(
                edits.len() == responses.len() && edits.iter().zip(&responses).all(|(e, r)| e["raw_response_sha256"] == r.raw_response_sha256.as_str()),
                "{}: request {rid}'s item check read other raw children than the accepted ones",
                it.item_id
            );
        }
    }
    let raws: Vec<&str> = found.iter().map(|f| f.3.unwrap_or_default()).collect();
    let (text, ai_spans, share) = rebuild(ptext, &it.kind, &it.source_spans, &raws).with_context(|| format!("{}: raw twin", it.item_id))?;
    Ok(Some(RawDoc { text, ai_spans, share, raws: raws.iter().map(|r| r.to_string()).collect(), responses }))
}

/// An item's screen-fail stratum outcome.
#[derive(Clone, Debug)]
pub enum ScreenFail {
    /// The document, inside the window.
    Doc(RawDoc),
    /// Left out: the window's reasons (with the document's words) or an
    /// assembly error.
    LeftOut { reasons: Vec<String>, words: Option<usize>, responses: Vec<RawResponse> },
}

/// The item's `rform_screen_fail_v1` outcome (see the module notes), whatever
/// the item's own outcome. `Ok(None)` when the item is not in the stratum: a
/// request with no raw response, no rejected first raw response, or a
/// rejection that is not the form screen's alone (one before the profile
/// step included). An error when a first raw response in the stratum
/// carries no raw child, or a raw text or the parent fails its digest.
pub fn screen_fail(it: &Item, ptext: &str, states: &HashMap<String, ReqState>) -> Result<Option<ScreenFail>> {
    let mut first: Vec<(&str, u32, &Value)> = Vec::new();
    for rid in &it.requests {
        let Some((a, line)) = states.get(rid).and_then(first_raw_response) else {
            return Ok(None);
        };
        first.push((rid, a, line));
    }
    let mut rejected = first.iter().filter(|f| f.2["verdict"]["accepted"] != true).peekable();
    if rejected.peek().is_none() || !rejected.all(|f| failed_form_screen_only(&f.2["verdict"])) {
        return Ok(None);
    }
    check_parent(it, ptext)?;
    let mut raws: Vec<&str> = Vec::new();
    for (rid, a, line) in &first {
        let raw = raw_child(&line["verdict"]).with_context(|| format!("{}: request {rid} attempt {a}", it.item_id))?;
        raws.push(raw.with_context(|| format!("{}: request {rid} attempt {a} has no raw child", it.item_id))?);
    }
    let responses: Vec<RawResponse> = first.iter().map(|(rid, a, line)| RawResponse::of(rid, *a, line)).collect();
    Ok(Some(match rebuild(ptext, &it.kind, &it.source_spans, &raws) {
        Err(e) => ScreenFail::LeftOut { reasons: vec![format!("assembly_error:{e}")], words: None, responses },
        Ok((text, ai_spans, share)) => {
            let n = text::words(&text);
            let why = doc_window_reasons(n, LENGTH_WINDOW);
            if why.is_empty() {
                ScreenFail::Doc(RawDoc { text, ai_spans, share, raws: raws.iter().map(|r| r.to_string()).collect(), responses })
            } else {
                ScreenFail::LeftOut { reasons: why, words: Some(n), responses }
            }
        }
    }))
}

fn rform_doc<'a>(d: &'a Value, stratum: &str) -> Option<&'a Value> {
    (d["form"] == R_FORM && d["stratum"] == stratum && d["text"].is_string()).then_some(d)
}

/// An N-form record's raw twin. N0's and S0-13's positives are these only
/// (amendment 19 item 19.5).
pub fn raw_twin_doc(entry: &Value) -> Option<&Value> {
    rform_doc(&entry[RAW_TWIN_FIELD], RAW_TWIN_STRATUM)
}

/// The screen-fail stratum document of a record, failure or exclusion (not
/// a left-out note). It enters only the R-form evaluation populations
/// (Dev-iter-2, Dev-select-2, Test-2; amendment 19 item 19.5),
/// never N0, S0-13 or a Train or CAL population.
pub fn screen_fail_doc(entry: &Value) -> Option<&Value> {
    rform_doc(&entry[SCREEN_FAIL_ID], SCREEN_FAIL_ID)
}

/// Refuses an R-form document whose text an earlier one of the run holds
/// (amendment 19 item 19.6); `seen` maps text digests to ids.
fn unique_rform(seen: &mut HashMap<String, String>, d: &Value) -> Result<()> {
    let (sha, id) = (d["text_sha256"].as_str().context("R-form text_sha256")?, d["id"].as_str().context("R-form id")?);
    if let Some(first) = seen.get(sha) {
        anyhow::bail!("duplicate R-form text: {id} has the text of {first}");
    }
    seen.insert(sha.to_string(), id.to_string());
    Ok(())
}

/// Form-sensitivity pairs (protocol A2 §11.7): (N-form id, R-form id) for
/// each raw twin whose `twin_of` is one of `n_records`' ids, sorted. The
/// screen-fail stratum has no N-form twin and is never paired.
pub fn fs_pairs<'a>(n_records: impl IntoIterator<Item = &'a Value>, r_docs: impl IntoIterator<Item = &'a Value>) -> Vec<(String, String)> {
    let ids: BTreeSet<&str> = n_records.into_iter().filter_map(|r| r["id"].as_str()).collect();
    let mut out: Vec<(String, String)> = r_docs
        .into_iter()
        .filter(|r| r["stratum"] == RAW_TWIN_STRATUM)
        .filter_map(|r| Some((r["twin_of"].as_str().filter(|t| ids.contains(t))?.to_string(), r["id"].as_str()?.to_string())))
        .collect();
    out.sort();
    out
}

// ------------------------------------------------------------------ records

/// What every document of one item shares in its record.
struct ItemCtx<'a> {
    it: &'a Item,
    ptext: &'a str,
    prow: &'a Value,
    g: &'a Value,
    req0: &'a Request,
    prompt_shas: &'a [String],
    attempts: &'a [Value],
    plan_sha: &'a str,
}

/// A positive record of one document of the item: `text` with its final
/// spans, realised share, replacements and token count.
fn record(c: &ItemCtx, text: &str, ai_spans: &[(usize, usize)], share: f64, reps: &[&str], ntok: usize) -> Result<Value> {
    let it = c.it;
    let origin = if it.kind == "mirror" { "model_only" } else { "mixed" };
    let sb = if it.kind == "mirror" { "model_only" } else { share_bin(share) };
    let copies_parent = reps.iter().any(|r| it.kind != "mirror" && overlap::shares_window(r, c.ptext, 50));
    // Plan v3 span-supervision cohorts: exact per-word labels (0 human,
    // 1 model) as run lengths under the word policy.
    let word_runs = if crate::span_ops::COHORTS.contains(&it.cohort.as_str()) {
        Some(json!(crate::span_ops::label_runs(&crate::span_ops::word_labels(text, ai_spans)?)))
    } else {
        None
    };
    let mut rec = json!({
        "schema": RECORD_SCHEMA,
        "id": it.item_id,
        "frozen_split": it.split,
        "split": it.split,
        "domain": it.domain,
        "collection": c.prow["collection"],
        "source_group": it.source_group,
        "source_group_policy": "inherits the parent human document's source family and split",
        "writer_key": c.prow["writer_key"],
        "parent_id": it.parent_id,
        "parent_text_sha256": it.parent_text_sha256,
        "origin": origin,
        "human_role_present": origin == "mixed",
        "ai_role_present": true,
        "cohort": it.cohort,
        "kind": it.kind,
        "population": match it.cohort.as_str() {
            "primary_mirror" | "primary_mixed" => "primary",
            "diverse_mirror" | "diverse_mixed" if it.split == "train" => "train_diverse",
            "diverse_mirror" | "diverse_mixed" => "supplementary_eval_diverse_not_gated",
            "train_base_robustness" | "train_base_v2" | "train_base_v3" => "robustness_train_only",
            "test_robustness" => "robustness_test_only_outside_primary",
            "prefix_continuation" | "middle_rewrite" if it.split == "train" => "train_span_supervision",
            "prefix_continuation" | "middle_rewrite" => "supplementary_eval_span_supervision_not_gated",
            _ => "supplementary_train_only",
        },
        "requested_bin": it.bin,
        "requested_target_bp": it.target_bp,
        "requested_fraction_of_source_words": it.requested_fraction,
        "anchor": it.anchor,
        "multi_span": it.multi_span,
        "plan_notes": it.notes,
        "text": text,
        "text_sha256": text::sha_hex(text.as_bytes()),
        "word_count": text::words(text),
        "word_policy": text::WORD_POLICY,
        "a_tokens_with_specials": ntok,
        "share_target": share,
        "share_bin": sb,
        "ai_byte_spans": ai_spans,
        "share_evidence": {
            "schema": "slop_ninja_realized_span_share_v2",
            "basis": "realised final-word fraction in model-produced spans; never the requested fraction",
            "source_byte_spans": it.source_spans,
            "replacements": reps,
            "final_byte_spans": ai_spans,
            "replacement_words": reps.iter().map(|r| text::words(r)).sum::<usize>(),
            "final_words": text::words(text),
            "full_rewrite_is_mixed_history": it.kind == "full",
            "mirror_is_model_only": it.kind == "mirror",
            "replacement_shares_50_word_window_with_parent": copies_parent,
        },
        "generation": {
            "generator_key": it.generator,
            "generator": c.g,
            "endpoint": c.req0.endpoint,
            "prompt_template_id": it.template_id,
            "operation": it.operation,
            "style": it.style,
            "prompt_sha256": c.prompt_shas,
            "decoding": it.decoding,
            "requests": c.attempts,
            "ranks": it.ranks,
            "plan_sha256": c.plan_sha,
        },
        "rights": {"inherits_parent": true, "parent_rights": c.prow["rights"],
            "generated_text": "local open-weight generation; model licence in generation.generator; no detector output used"},
        "detector_outputs_used": false,
    });
    if let Some(w) = word_runs {
        rec["share_evidence"]["word_label_runs"] = w;
    }
    Ok(rec)
}

/// An R-form record of the item: `record` over the raw document, with its
/// id, `form`, `stratum`, `twin_of` and the `rform` evidence.
fn rform_record(c: &ItemCtx, d: &RawDoc, tok: &Tokenizer, id: String, stratum: &str, twin_of: Option<&str>, evidence: Value) -> Result<Value> {
    let ntok = generation::a_tokens(tok, &d.text)?;
    let raws: Vec<&str> = d.raws.iter().map(String::as_str).collect();
    let mut rec = record(c, &d.text, &d.ai_spans, d.share, &raws, ntok)?;
    rec["id"] = json!(id);
    rec["form"] = json!(R_FORM);
    rec["stratum"] = json!(stratum);
    rec["twin_of"] = json!(twin_of);
    rec["rform"] = evidence;
    Ok(rec)
}

/// Puts an item's stratum outcome under `rform_screen_fail_v1` in its entry,
/// refusing a document whose text an earlier R-form document holds.
fn put_stratum(entry: &mut Value, s: Option<Value>, seen: &mut HashMap<String, String>) -> Result<()> {
    if let Some(s) = s {
        if rform_doc(&s, SCREEN_FAIL_ID).is_some() {
            unique_rform(seen, &s)?;
        }
        entry[SCREEN_FAIL_ID] = s;
    }
    Ok(())
}

fn responses_record(rs: &[RawResponse]) -> Value {
    json!(rs.iter().map(RawResponse::record).collect::<Vec<_>>())
}

#[allow(clippy::too_many_arguments)]
pub fn finalize(
    items: &[Item],
    requests: &HashMap<String, Request>,
    states: &HashMap<String, ReqState>,
    humans: &HashMap<String, (String, Value)>,
    generators: &HashMap<String, Value>,
    plan_failures: &[Value],
    tok: &Tokenizer,
    plan_sha: &str,
) -> Result<Out> {
    let mut records: BTreeMap<String, Vec<Value>> = BTreeMap::new();
    let mut failures: BTreeMap<String, Vec<Value>> = BTreeMap::new();
    let mut exclusions: BTreeMap<String, Vec<Value>> = BTreeMap::new();
    let mut seen_norm: HashSet<u64> = HashSet::new();
    let mut seen_ws: HashSet<u64> = HashSet::new();
    // R-form text digests -> the first document's id (amendment 19 item 19.6)
    let mut seen_rform: HashMap<String, String> = HashMap::new();
    let mut noop_ok = 0usize;
    let mut noop_bad = 0usize;
    // counters[split][dimension][value] -> n
    let mut c: BTreeMap<String, BTreeMap<String, BTreeMap<String, usize>>> = BTreeMap::new();
    let mut bump = |split: &str, dim: &str, val: &str| {
        *c.entry(split.into()).or_default().entry(dim.into()).or_default().entry(val.into()).or_default() += 1;
    };
    for f in plan_failures {
        let split = f["split"].as_str().unwrap_or("?").to_owned();
        bump(&split, "status", "plan_failure");
        failures.entry(split).or_default().push(f.clone());
    }
    for it in items {
        let (ptext, prow) = humans.get(&it.parent_id).context("parent")?;
        bump(&it.split, "planned_by_cohort", &it.cohort);
        bump(&it.split, "planned_by_generator", &it.generator);
        // no-op splice control
        if !it.source_spans.is_empty() {
            let orig: Vec<&str> = it.source_spans.iter().map(|&(s, e)| &ptext[s..e]).collect();
            match generation::assemble(ptext, &it.source_spans, &orig) {
                Ok((t, _, _)) if t == *ptext => noop_ok += 1,
                _ => noop_bad += 1,
            }
        }
        let mut reps: Vec<String> = Vec::new();
        let mut attempts = Vec::new();
        let mut status = "accepted";
        for rid in &it.requests {
            let r = &requests[rid];
            let st = states.get(rid).cloned().unwrap_or_default();
            let tried: Vec<Value> = st
                .responses
                .iter()
                .map(|(a, v)| json!({"attempt":a,"seed":v["seed"],"status":v["status"],"elapsed_ms":v["elapsed_ms"],
                    "accepted":v["verdict"]["accepted"],"reasons":v["verdict"]["reasons"],"diag":v["verdict"]["diag"]}))
                .collect();
            let uncertain: Vec<u32> = st.intents.iter().copied().filter(|a| !st.responses.contains_key(a)).collect();
            attempts.push(json!({"request_id":rid,"span_index":r.span_index,"seed0":r.seed,"request_sha256":r.request_sha256,
                "attempts":tried,"uncertain_attempts":uncertain}));
            match st.accepted() {
                Some(v) => reps.push(v["verdict"]["text"].as_str().context("accepted text")?.to_owned()),
                None => {
                    if st.done() {
                        status = "terminal_failure";
                    } else if status == "accepted" {
                        status = "not_run";
                    }
                }
            }
        }
        let g = generators.get(&it.generator).cloned().unwrap_or(Value::Null);
        ensure!(!it.requests.is_empty(), "{}: item has no request", it.item_id);
        let req0 = &requests[&it.requests[0]];
        let prompt_shas: Vec<String> = it.requests.iter().map(|r| text::sha_hex(requests[r].prompt.as_bytes())).collect();
        let ic = ItemCtx { it, ptext, prow, g: &g, req0, prompt_shas: &prompt_shas, attempts: &attempts, plan_sha };
        // The screen-fail stratum reads the item's first raw responses,
        // whatever the item's outcome.
        let stratum = match screen_fail(it, ptext, states)? {
            None => None,
            Some(ScreenFail::Doc(d)) => {
                bump(&it.split, SCREEN_FAIL_ID, "emitted");
                let ev = json!({"rule": SCREEN_FAIL_RULE, "parent_text_sha256": it.parent_text_sha256, "raw_responses": responses_record(&d.responses),
                    "length_window": {"id": LENGTH_WINDOW_ID, "min_words": LENGTH_WINDOW.0, "max_words": LENGTH_WINDOW.1, "words": text::words(&d.text), "pass": true}});
                Some(rform_record(&ic, &d, tok, format!("{SCREEN_FAIL_ID}:{}", it.item_id), SCREEN_FAIL_ID, None, ev)?)
            }
            Some(ScreenFail::LeftOut { reasons, words, responses }) => {
                for r in &reasons {
                    bump(&it.split, SCREEN_FAIL_ID, &format!("left_out:{}", r.split(':').next().unwrap_or(r)));
                }
                Some(json!({"stratum": SCREEN_FAIL_ID, "id": format!("{SCREEN_FAIL_ID}:{}", it.item_id), "left_out": true, "reasons": reasons,
                    "words": words, "length_window": {"id": LENGTH_WINDOW_ID, "min_words": LENGTH_WINDOW.0, "max_words": LENGTH_WINDOW.1},
                    "raw_responses": responses_record(&responses), "parent_text_sha256": it.parent_text_sha256, "rule": SCREEN_FAIL_RULE}))
            }
        };
        if status != "accepted" {
            bump(&it.split, "status", status);
            bump(&it.split, &format!("{status}_by_generator"), &it.generator);
            let mut f = json!({"item_id":it.item_id,"split":it.split,
                "parent_id":it.parent_id,"cohort":it.cohort,"kind":it.kind,"bin":it.bin,"generator":it.generator,
                "stage":"generation","status":status,"requests":attempts});
            put_stratum(&mut f, stratum, &mut seen_rform)?;
            failures.entry(it.split.clone()).or_default().push(f);
            continue;
        }
        let rep_refs: Vec<&str> = reps.iter().map(String::as_str).collect();
        let (final_text, ai_spans, share) = if it.kind == "mirror" {
            (reps[0].clone(), vec![(0, reps[0].len())], 1.0)
        } else {
            generation::assemble(ptext, &it.source_spans, &rep_refs)?
        };
        let ntok = generation::a_tokens(tok, &final_text)?;
        // global generated-text dedup in plan order
        let mut dup = Vec::new();
        for r in &rep_refs {
            let k = grammar_a_pools::text::text_keys(r)[1];
            if seen_norm.contains(&k) {
                dup.push("generated_normalized_text");
            }
            if grammar_a_pools::text::shingles_ws(r).iter().any(|x| seen_ws.contains(x)) {
                dup.push("generated_50_word_sequence");
            }
        }
        if ntok > generation::MAX_A_TOKENS {
            dup.push("assembled_over_2048_a_tokens");
        }
        if !dup.is_empty() {
            dup.sort();
            dup.dedup();
            bump(&it.split, "status", "excluded_in_finalize");
            let mut x = json!({"item_id":it.item_id,"split":it.split,
                "parent_id":it.parent_id,"cohort":it.cohort,"generator":it.generator,"reasons":dup,"requests":attempts});
            put_stratum(&mut x, stratum, &mut seen_rform)?;
            exclusions.entry(it.split.clone()).or_default().push(x);
            continue;
        }
        for r in &rep_refs {
            seen_norm.insert(grammar_a_pools::text::text_keys(r)[1]);
            seen_ws.extend(grammar_a_pools::text::shingles_ws(r));
        }
        let sb = if it.kind == "mirror" { "model_only" } else { share_bin(share) };
        bump(&it.split, "status", "accepted");
        bump(&it.split, "accepted_by_cohort", &it.cohort);
        bump(&it.split, "accepted_by_generator", &it.generator);
        bump(&it.split, &format!("accepted_share_bin:{}", it.cohort), sb);
        if let Some(b) = &it.bin {
            bump(&it.split, &format!("accepted_requested_bin:{}", it.cohort), b);
        }
        bump(&it.split, "accepted_by_domain", &it.domain);
        if let Some(st) = &it.style {
            let dt = st["document_type"].as_str().unwrap_or("-");
            let op = it.operation.as_deref().unwrap_or(it.kind.as_str());
            let voice = st["style_kind"].as_str().unwrap_or("-");
            bump(&it.split, &format!("accepted_cell:{}", it.cohort), &format!("{}|{}|{}|{}|{}", it.generator, op, dt, sb, voice));
            bump(&it.split, &format!("accepted_operation:{}", it.cohort), op);
            bump(&it.split, &format!("accepted_document_type:{}", it.cohort), dt);
            bump(&it.split, &format!("accepted_voice:{}", it.cohort), voice);
        }
        let fam = g["family"].as_str().unwrap_or("?").to_owned();
        bump(&it.split, "accepted_by_family", &fam);
        let mut rec = record(&ic, &final_text, &ai_spans, share, &rep_refs, ntok)?;
        if let Some(tw) = raw_twin(it, ptext, states)? {
            let same = grammar_a_pools::text::folded_tokens(&tw.text) == grammar_a_pools::text::folded_tokens(&final_text);
            bump(&it.split, "rform_raw_twin", "emitted");
            bump(&it.split, "rform_raw_twin_folded_tokens", if same { "equal_n_form" } else { "differ_from_n_form" });
            let ev = json!({"rule": RAW_TWIN_RULE, "parent_text_sha256": it.parent_text_sha256, "raw_responses": responses_record(&tw.responses),
                "n_form_text_sha256": text::sha_hex(final_text.as_bytes()), "folded_tokens_equal_n_form": same,
                "window": "the N-form document's verdict (protocol A2 section 7.2)"});
            let twin = rform_record(&ic, &tw, tok, format!("{RAW_TWIN_ID_PREFIX}{}", it.item_id), RAW_TWIN_STRATUM, Some(&it.item_id), ev)?;
            unique_rform(&mut seen_rform, &twin)?;
            rec[RAW_TWIN_FIELD] = twin;
        }
        put_stratum(&mut rec, stratum, &mut seen_rform)?;
        records.entry(it.split.clone()).or_default().push(rec);
    }
    let summary = json!({"counts":c,"noop_splice_control":{"exact_reconstructions":noop_ok,"mismatches":noop_bad}});
    Ok(Out { records, failures, exclusions, summary })
}
#[cfg(test)]
mod tests {
    //! Synthetic text only (no corpus text), no model, detector or network
    //! call: verdicts come from the acceptance functions themselves.
    use super::*;
    use crate::form_post::{self, FormPost, ItemSpec};
    use crate::generation::{Ctx, VerdictFn};
    use crate::overlap::{Overlap, PriorIndex};
    use crate::plan;

    const SYN: [&str; 20] = [
        "amber", "basalt", "cobalt", "delta", "ember", "fjord", "garnet", "harbor", "indigo", "juniper", "kestrel", "lantern", "meadow",
        "nectar", "orchid", "pebble", "quartz", "river", "saffron", "timber",
    ];
    const ALT: [&str; 20] = [
        "acorn", "bramble", "canyon", "dune", "estuary", "fern", "glacier", "heather", "inlet", "jasper", "knoll", "lichen", "marsh",
        "nettle", "oasis", "prairie", "quarry", "ridge", "sorrel", "thicket",
    ];

    /// `n` sentences of `len` words drawn by a seeded LCG, capitalised and
    /// ending with a full stop, joined by single spaces; in the first
    /// `dashed` sentences words 2 and 3 are joined by a closed em dash.
    fn prose_d(vocab: &[&str], seed: u64, n: usize, len: usize, dashed: usize) -> String {
        let mut st = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        let mut out = Vec::new();
        for k in 0..n {
            let mut s = String::new();
            for i in 0..len {
                st = st.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                if i > 0 {
                    s.push_str(if i == 2 && k < dashed { "\u{2014}" } else { " " });
                }
                s.push_str(vocab[((st >> 33) as usize) % vocab.len()]);
            }
            out.push(format!("{}{}.", s[..1].to_uppercase(), &s[1..]));
        }
        out.join(" ")
    }

    fn prose(vocab: &[&str], seed: u64, n: usize, len: usize) -> String {
        prose_d(vocab, seed, n, len, 0)
    }

    /// P1: three paragraphs, about 194 words; curly double quotes and spaced
    /// em dashes are its majorities.
    fn parent_one() -> String {
        let a = format!("{} The keeper called it \u{201C}amber harbor\u{201D} again \u{2014} twice. {}", prose(&SYN, 1, 3, 11), prose(&SYN, 2, 2, 10));
        let b = prose(&SYN, 3, 6, 11);
        let c = format!("{} They named it \u{201C}quiet fjord\u{201D} later \u{2014} much later. {}", prose(&SYN, 4, 2, 12), prose(&SYN, 5, 3, 11));
        format!("{a}\n\n{b}\n\n{c}")
    }

    /// P2: two paragraphs, about 165 words; spaced em dashes are its majority.
    fn parent_two() -> String {
        let a = format!("{} The pier stayed open \u{2014} briefly. {}", prose(&SYN, 61, 4, 11), prose(&SYN, 62, 3, 11));
        let b = format!("{} The tide turned \u{2014} slowly. {}", prose(&SYN, 63, 3, 11), prose(&SYN, 64, 4, 11));
        format!("{a}\n\n{b}")
    }

    fn sentence(p: &str, k: usize) -> (usize, usize) {
        let s = text::sentences(p)[k];
        (s.start, s.end)
    }

    fn tok() -> Tokenizer {
        r#"{"version":"1.0","truncation":null,"padding":null,"added_tokens":[],"normalizer":null,"pre_tokenizer":{"type":"Whitespace"},"post_processor":null,"decoder":null,"model":{"type":"WordLevel","vocab":{"[UNK]":0},"unk_token":"[UNK]"}}"#
            .parse()
            .unwrap()
    }

    /// One item with its requests (one per span; one for a mirror) and each
    /// request's reply per attempt.
    struct Case {
        item: Item,
        requests: Vec<Request>,
        replies: Vec<Vec<String>>,
    }

    fn case(id: &str, parent_id: &str, parent: &str, spans: Vec<(usize, usize)>, replies: Vec<Vec<String>>) -> Case {
        let kind = if spans.is_empty() { "mirror" } else { "span" };
        let cohort = if spans.is_empty() { "primary_mirror" } else { "primary_mixed" };
        let decoding = plan::profile("A", 0.7, 0.9, 1536);
        let mut requests = Vec::new();
        let targets: Vec<(Option<(usize, usize)>, usize)> =
            if spans.is_empty() { vec![(None, text::words(parent))] } else { spans.iter().map(|&(s, e)| (Some((s, e)), text::words(&parent[s..e]))).collect() };
        for (k, (span, target)) in targets.into_iter().enumerate() {
            let rid = format!("{id}-r{k}");
            requests.push(Request {
                request_id: rid.clone(),
                item_id: id.into(),
                parent_id: parent_id.into(),
                split: "train".into(),
                generator: "g".into(),
                endpoint: "chat".into(),
                template_id: "t".into(),
                prompt: format!("prompt {rid}"),
                span_index: k,
                source_span: span,
                target_words: target,
                cut_budget: None,
                decoding: decoding.clone(),
                request_sha256: text::sha_hex(rid.as_bytes()),
                seed: 7,
                op: None,
                cohort: Some(cohort.into()),
            });
        }
        assert_eq!(requests.len(), replies.len());
        let item = Item {
            item_id: id.into(),
            split: "train".into(),
            domain: "synthetic_domain".into(),
            parent_id: parent_id.into(),
            parent_text_sha256: text::sha_hex(parent.as_bytes()),
            source_group: format!("g-{parent_id}"),
            cohort: cohort.into(),
            kind: kind.into(),
            bin: (kind == "span").then(|| "R2".to_string()),
            target_bp: None,
            anchor: None,
            multi_span: spans.len() > 1,
            generator: "g".into(),
            decoding,
            template_id: "t".into(),
            source_spans: spans,
            requested_fraction: None,
            requests: requests.iter().map(|r| r.request_id.clone()).collect(),
            notes: vec![],
            ranks: BTreeMap::new(),
            operation: None,
            style: None,
        };
        Case { item, requests, replies }
    }

    /// The battery over P1 and P2 (see each line).
    fn battery(p1: &str, p2: &str) -> Vec<Case> {
        let s = |x: &str| vec![x.to_string()];
        let title = |t: &str, body: String| format!("{t}\n{body}");
        vec![
            // a mirror in ASCII quotes, one paragraph: accepted at attempt 0
            case("mirror-a", "p1", p1, vec![], vec![s(&format!("{} The ranger said \"marsh nettle\" twice. {}", prose(&ALT, 11, 9, 11), prose(&ALT, 12, 8, 11)))]),
            // a span in ASCII quotes: accepted at attempt 0
            case("span-b", "p1", p1, vec![sentence(p1, 1)], vec![s("The ranger said \"marsh nettle\" and walked on past the oasis ridge.")]),
            // two spans, one item: each accepted at attempt 0, the second judged on the item
            case("multi-c", "p1", p1, vec![sentence(p1, 2), sentence(p1, 13)], vec![s("The warden said \"quiet inlet\" and rowed past the glacier knoll."), s(&prose(&ALT, 33, 1, 12))]),
            // a mirror whose first response has a title line (form screen only), then accepted
            case("screen-d", "p1", p1, vec![], vec![vec![title("A Quiet Harbor Morning", prose(&ALT, 41, 17, 11)), prose(&ALT, 42, 18, 11)]]),
            // a mirror of P2 failing the form screen twice; its first raw response
            // is under 150 words, its laid-out child (spaced dashes) is not
            case("screen-e", "p2", p2, vec![], vec![vec![title("Quiet Harbor Notes", prose_d(&ALT, 51, 13, 11, 6)), title("Quiet Harbor Notes", prose_d(&ALT, 52, 13, 11, 6))]]),
            // a mirror echoing its parent first (the echo screen, not the form screen), then accepted
            case("echo-g", "p1", p1, vec![], vec![vec![p1.to_string(), prose(&ALT, 71, 18, 11)]]),
            // a span whose first response is a list (form screen only), then accepted
            case("span-f", "p1", p1, vec![sentence(p1, 9)], vec![vec![
                "- Acorn bramble canyon dune estuary fern.\n- Glacier heather inlet jasper knoll lichen.".to_string(),
                "Lichen and fern covered the old marsh wall near the ridge.".to_string(),
            ]]),
        ]
    }

    fn parents() -> HashMap<String, String> {
        [("p1".to_string(), parent_one()), ("p2".to_string(), parent_two())].into()
    }

    fn chat(reply: &str) -> Value {
        json!({"choices":[{"message":{"content":reply},"finish_reason":"stop"}]})
    }

    /// A reply that stands for a chat body with no content (`no_content`).
    const NO_CONTENT_REPLY: &str = "\u{0}no content";
    /// A reply that stands for an HTTP 500 with an error body.
    const HTTP_500_REPLY: &str = "\u{0}http 500";

    /// The status and body of one synthetic reply.
    fn body(reply: &str) -> (u16, Value) {
        match reply {
            NO_CONTENT_REPLY => (200, json!({"choices":[{"message":{},"finish_reason":"stop"}]})),
            HTTP_500_REPLY => (500, json!({"error":{"message":"synthetic"}})),
            _ => (200, chat(reply)),
        }
    }

    /// One request's attempts through `vf` in order until one is accepted,
    /// as `generation::run` journals them.
    fn judge(ctx: &Ctx, vf: &VerdictFn, r: &Request, replies: &[String]) -> ReqState {
        let mut st = ReqState::default();
        for (a, reply) in replies.iter().enumerate().take(generation::ATTEMPTS as usize) {
            let a = a as u32;
            st.intents.push(a);
            let (status, parsed) = body(reply);
            let v = vf(ctx, r, status, &parsed);
            let ok = v["accepted"] == true;
            st.responses.insert(a, json!({"kind":"response","request_id":r.request_id,"item_id":r.item_id,"split":r.split,"generator":r.generator,
                "attempt":a,"seed":r.seed.wrapping_add(a),"status":status,"elapsed_ms":0,"raw_body":"","verdict":v}));
            if ok {
                break;
            }
        }
        st
    }

    /// The profile v2 step of population `deviter2_t` over P1 and P2, with
    /// the battery's items with several requests bound.
    fn form(ps: &HashMap<String, String>, cases: &[Case]) -> FormPost {
        let specs: Vec<ItemSpec> = cases
            .iter()
            .map(|c| ItemSpec { item_id: c.item.item_id.clone(), parts: c.requests.iter().map(|r| (r.request_id.clone(), r.source_span.unwrap_or((0, 0)), None)).collect() })
            .collect();
        FormPost::new("deviter2_t", [("p1", "synthetic_domain", ps["p1"].as_str()), ("p2", "synthetic_domain", ps["p2"].as_str())])
            .unwrap()
            .with_items(specs, &HashMap::new())
            .unwrap()
    }

    /// Every request of the battery judged in order, with the profile off
    /// (`None`) or under profile v2.
    fn states(ps: &HashMap<String, String>, cases: &[Case], post: Option<FormPost>) -> HashMap<String, ReqState> {
        let ((post, doc_words), vf) = form_post::generate_wiring(post).unwrap();
        let ctx = Ctx {
            endpoint: String::new(),
            alias: "m".into(),
            config: json!({}),
            tokenizer: tok(),
            overlap: Overlap::new(PriorIndex::empty()),
            parents: ps.clone(),
            client: generation::client().unwrap(),
            post,
            doc_words,
        };
        let mut out = HashMap::new();
        for c in cases {
            for (r, rep) in c.requests.iter().zip(&c.replies) {
                out.insert(r.request_id.clone(), judge(&ctx, &*vf, r, rep));
            }
        }
        out
    }

    fn run(ps: &HashMap<String, String>, cases: &[Case], st: &HashMap<String, ReqState>) -> Result<Out> {
        let items: Vec<Item> = cases.iter().map(|c| c.item.clone()).collect();
        let requests: HashMap<String, Request> = cases.iter().flat_map(|c| c.requests.iter().map(|r| (r.request_id.clone(), r.clone()))).collect();
        let humans: HashMap<String, (String, Value)> =
            ps.iter().map(|(k, t)| (k.clone(), (t.clone(), json!({"collection": "synthetic", "writer_key": format!("w-{k}"), "rights": "synthetic"})))).collect();
        finalize(&items, &requests, st, &humans, &HashMap::new(), &[], &tok(), "plan")
    }

    /// An entry's R-form documents: its raw twin, then its stratum document.
    fn rform_docs(e: &Value) -> Vec<&Value> {
        raw_twin_doc(e).into_iter().chain(screen_fail_doc(e)).collect()
    }

    fn digest(o: &Out) -> String {
        text::sha_hex(&serde_json::to_vec(&json!({"records": o.records, "failures": o.failures, "exclusions": o.exclusions, "summary": o.summary})).unwrap())
    }

    /// The digest of finalize's outputs on the battery with the profile off,
    /// recorded by this test from the code before raw twins existed.
    const OFF_GOLDEN: &str = "ed9bd2a24fa7a139e5dd10a2c60a8e31f53c48d6450f42e5051aba08358e1ae1";

    #[test]
    fn finalize_profile_off_outputs_are_the_pre_twin_bytes() {
        let ps = parents();
        let cases = battery(&ps["p1"], &ps["p2"]);
        let st = states(&ps, &cases, None);
        let o = run(&ps, &cases, &st).unwrap();
        let d = digest(&o);
        eprintln!("OFF_DIGEST {d}");
        assert_eq!(d, OFF_GOLDEN);
        // and no R-form field or counter, explicitly
        for e in o.records.values().chain(o.failures.values()).chain(o.exclusions.values()).flatten() {
            assert!(e.get(RAW_TWIN_FIELD).is_none() && e.get(SCREEN_FAIL_ID).is_none(), "{}", e["id"]);
            assert!(raw_twin_doc(e).is_none() && screen_fail_doc(e).is_none());
        }
        assert!(!o.summary.to_string().contains("rform"));
    }

    fn v2(ps: &HashMap<String, String>, cases: &[Case]) -> HashMap<String, ReqState> {
        states(ps, cases, Some(form(ps, cases)))
    }

    fn rec<'a>(o: &'a Out, id: &str) -> &'a Value {
        o.records["train"].iter().find(|r| r["id"] == id).unwrap_or_else(|| panic!("no record {id}"))
    }

    fn accepted_raw<'a>(st: &'a HashMap<String, ReqState>, rid: &str) -> &'a str {
        st[rid].accepted().unwrap()["verdict"]["raw_text"].as_str().unwrap()
    }

    fn sha(s: &str) -> String {
        text::sha_hex(s.as_bytes())
    }

    /// No sentence of four or more words from `texts` appears in `v`.
    fn assert_no_text(v: &Value, texts: &[&str]) {
        let s = v.to_string();
        for t in texts {
            for sent in text::sentences(t) {
                let x = &t[sent.start..sent.end];
                if text::words(x) >= 4 {
                    assert!(!s.contains(x), "holds text: {x:?}");
                }
            }
        }
    }

    #[test]
    fn finalize_v2_raw_twins_rebuild_byte_exactly_from_parent_and_raw_responses() {
        let ps = parents();
        let cases = battery(&ps["p1"], &ps["p2"]);
        let st = v2(&ps, &cases);
        let o = run(&ps, &cases, &st).unwrap();
        let ids: Vec<&str> = o.records["train"].iter().map(|r| r["id"].as_str().unwrap()).collect();
        assert_eq!(ids, ["mirror-a", "span-b", "multi-c", "screen-d", "echo-g", "span-f"]);
        for c in &cases {
            let Some(n) = o.records["train"].iter().find(|r| r["id"] == c.item.item_id.as_str()) else { continue };
            let p = &ps[&c.item.parent_id];
            let tw = &n[RAW_TWIN_FIELD];
            // the twin is rebuilt from the parent and the accepted raw responses, byte for byte
            let raws: Vec<&str> = c.requests.iter().map(|r| accepted_raw(&st, &r.request_id)).collect();
            let want = if c.item.kind == "mirror" { raws[0].to_string() } else { generation::assemble(p, &c.item.source_spans, &raws).unwrap().0 };
            assert_eq!(tw["text"].as_str(), Some(want.as_str()), "{}", c.item.item_id);
            assert_eq!(tw["text_sha256"].as_str(), Some(sha(&want).as_str()));
            let (t2, s2, sh2) = rebuild(p, &c.item.kind, &c.item.source_spans, &raws).unwrap();
            assert_eq!((t2.as_str(), json!(s2), json!(sh2)), (want.as_str(), tw["ai_byte_spans"].clone(), tw["share_target"].clone()));
            // its spans hold exactly the raw responses, and its digests are theirs
            for (j, raw) in raws.iter().enumerate() {
                let sp = &tw["ai_byte_spans"][j];
                let (a, b) = (sp[0].as_u64().unwrap() as usize, sp[1].as_u64().unwrap() as usize);
                assert_eq!(&want[a..b], *raw);
                assert_eq!(tw["rform"]["raw_responses"][j]["raw_response_sha256"].as_str(), Some(sha(raw).as_str()));
                assert_eq!(tw["rform"]["raw_responses"][j]["request_id"].as_str(), Some(c.requests[j].request_id.as_str()));
            }
            // ids, form and the pair
            assert_eq!(tw["id"].as_str(), Some(format!("rform:{}", c.item.item_id).as_str()));
            assert_eq!((tw["form"].as_str(), tw["stratum"].as_str(), tw["twin_of"].as_str()), (Some(R_FORM), Some(RAW_TWIN_STRATUM), Some(c.item.item_id.as_str())));
            assert_eq!(tw["schema"], RECORD_SCHEMA);
            assert_eq!(tw["rform"]["n_form_text_sha256"], n["text_sha256"]);
            assert_eq!(tw["rform"]["parent_text_sha256"].as_str(), Some(sha(p).as_str()));
            assert_eq!(tw["rform"]["folded_tokens_equal_n_form"], true);
            // the N-form record's labels; measured fields are the twin document's
            for k in ["origin", "cohort", "kind", "population", "domain", "source_group", "parent_id", "writer_key", "generation", "rights", "split", "requested_bin"] {
                assert_eq!(tw[k], n[k], "{} {k}", c.item.item_id);
            }
            assert_eq!(tw["word_count"].as_u64(), Some(text::words(&want) as u64));
            assert_eq!(tw["share_evidence"]["replacements"], json!(raws));
            assert!(tw.get(RAW_TWIN_FIELD).is_none() && tw.get(SCREEN_FAIL_ID).is_none());
            if c.item.kind == "mirror" {
                assert_eq!((tw["share_target"].as_f64(), tw["share_bin"].as_str()), (Some(1.0), Some("model_only")));
            }
            assert_eq!(raw_twin_doc(n), Some(tw));
            assert!(screen_fail_doc(n).is_none_or(|d| d != tw));
        }
        // the twins are raw: ASCII quotes stay where the N-form records took the parent's curly ones
        for id in ["mirror-a", "span-b", "multi-c"] {
            let (n, t) = (rec(&o, id)["text"].as_str().unwrap(), rec(&o, id)[RAW_TWIN_FIELD]["text"].as_str().unwrap());
            assert!(t.contains('"') && !n.contains('"') && n.contains('\u{201C}'), "{id}");
            assert_ne!(n, t);
        }
        // a single-paragraph raw mirror stays one paragraph; its N-form child took P1's three
        let ma = rec(&o, "mirror-a");
        assert_eq!((ma["text"].as_str().unwrap().matches("\n\n").count(), ma[RAW_TWIN_FIELD]["text"].as_str().unwrap().matches("\n\n").count()), (2, 0));
        // the two-span item: both raw responses, and the item check read the same digests
        let mc = rec(&o, "multi-c");
        assert_eq!(mc[RAW_TWIN_FIELD]["ai_byte_spans"].as_array().unwrap().len(), 2);
        let last = &st["multi-c-r1"].accepted().unwrap()["verdict"]["diag"]["post"];
        assert_eq!(last["item_check"]["last"], true);
        for j in 0..2 {
            assert_eq!(last["edits"][j]["raw_response_sha256"], mc[RAW_TWIN_FIELD]["rform"]["raw_responses"][j]["raw_response_sha256"]);
        }
        assert_eq!(o.summary["counts"]["train"]["rform_raw_twin"], json!({"emitted": 6}));
        assert_eq!(o.summary["counts"]["train"]["rform_raw_twin_folded_tokens"], json!({"equal_n_form": 6}));
        // N-form records are unchanged by the twins apart from the two fields
        for n in &o.records["train"] {
            let mut m = n.clone();
            m.as_object_mut().unwrap().remove(RAW_TWIN_FIELD);
            m.as_object_mut().unwrap().remove(SCREEN_FAIL_ID);
            assert!(m.get("form").is_none() && m.get("twin_of").is_none() && m.get("rform").is_none());
        }
    }

    #[test]
    fn finalize_v2_screen_fail_stratum_takes_first_raw_responses_failing_the_form_screen_only() {
        let ps = parents();
        let (p1, p2) = (&ps["p1"], &ps["p2"]);
        let cases = battery(p1, p2);
        let st = v2(&ps, &cases);
        let o = run(&ps, &cases, &st).unwrap();
        let first_raw = |rid: &str| st[rid].responses[&0]["verdict"]["raw_text"].as_str().unwrap().to_string();
        // a mirror: the document is its first raw response, exactly; the item's accepted record keeps it
        let d = &rec(&o, "screen-d")[SCREEN_FAIL_ID];
        assert_eq!(d["text"].as_str(), Some(first_raw("screen-d-r0").as_str()));
        assert_eq!(d["text"].as_str().unwrap().lines().next(), Some("A Quiet Harbor Morning"));
        assert_eq!((d["id"].as_str(), d["form"].as_str(), d["stratum"].as_str()), (Some("rform_screen_fail_v1:screen-d"), Some(R_FORM), Some(SCREEN_FAIL_ID)));
        assert!(d["twin_of"].is_null());
        assert_eq!((d["origin"].as_str(), d["share_target"].as_f64()), (Some("model_only"), Some(1.0)));
        let r0 = &d["rform"]["raw_responses"][0];
        assert_eq!((r0["attempt"].as_u64(), r0["accepted"].as_bool()), (Some(0), Some(false)));
        assert_eq!(r0["reasons"], json!(["form_line_no_sentence_end"]));
        assert_eq!(r0["raw_response_sha256"].as_str(), Some(sha(&first_raw("screen-d-r0")).as_str()));
        assert_eq!(d["rform"]["length_window"]["words"].as_u64(), Some(191));
        assert_eq!(d["rform"]["length_window"]["pass"], true);
        // its twin is attempt 1's raw response, a different document
        assert_eq!(rec(&o, "screen-d")[RAW_TWIN_FIELD]["text"].as_str(), Some(accepted_raw(&st, "screen-d-r0")));
        assert_ne!(rec(&o, "screen-d")[RAW_TWIN_FIELD]["text"], d["text"]);
        // a span: the parent with the first raw response (a list) spliced in
        let f = &rec(&o, "span-f")[SCREEN_FAIL_ID];
        let want = generation::assemble(p1, &[sentence(p1, 9)], &[first_raw("span-f-r0").as_str()]).unwrap();
        assert_eq!(f["text"].as_str(), Some(want.0.as_str()));
        assert_eq!(f["ai_byte_spans"], json!(want.1));
        assert_eq!(f["origin"].as_str(), Some("mixed"));
        assert!(f["text"].as_str().unwrap().contains("\n- Glacier"));
        // a window failure leaves it out: the raw mirror has 140 words, its laid-out child 152
        let e = o.failures["train"].iter().find(|x| x["item_id"] == "screen-e").unwrap();
        assert_eq!(e["status"], "terminal_failure");
        let note = &e[SCREEN_FAIL_ID];
        assert_eq!(note["left_out"], true);
        assert_eq!(note["reasons"], json!(["final_document_below_150_words"]));
        assert_eq!(note["words"].as_u64(), Some(140));
        assert!(note.get("text").is_none() && screen_fail_doc(e).is_none() && raw_twin_doc(e).is_none());
        assert!(st["screen-e-r0"].responses[&0]["verdict"]["diag"]["post"]["length_window"]["words"].as_u64() == Some(152));
        assert_no_text(note, &[&first_raw("screen-e-r0"), p2]);
        // the echo screen, and first raw responses that were accepted, give no stratum
        for id in ["echo-g", "mirror-a", "span-b", "multi-c"] {
            assert!(rec(&o, id).get(SCREEN_FAIL_ID).is_none(), "{id}");
        }
        assert_eq!(o.summary["counts"]["train"][SCREEN_FAIL_ID], json!({"emitted": 2, "left_out:final_document_below_150_words": 1}));
        // every R-form document of the run
        let all: Vec<&Value> = o.records.values().chain(o.failures.values()).chain(o.exclusions.values()).flatten().flat_map(rform_docs).collect();
        assert_eq!(all.len(), 8);
        assert_eq!(all.iter().filter(|d| d["stratum"] == SCREEN_FAIL_ID).count(), 2);
    }

    #[test]
    fn finalize_v2_refuses_a_digest_mismatch() {
        let ps = parents();
        let cases = battery(&ps["p1"], &ps["p2"]);
        let st = v2(&ps, &cases);
        let err = |st: &HashMap<String, ReqState>| run(&ps, &cases, st).err().map(|e| format!("{e:#}")).unwrap_or_default();
        let edit = |rid: &str, a: u32, f: &dyn Fn(&mut Value)| {
            let mut s = st.clone();
            f(s.get_mut(rid).unwrap().responses.get_mut(&a).unwrap());
            s
        };
        // an accepted raw text that is not the digested one
        let e = err(&edit("mirror-a-r0", 0, &|v: &mut Value| {
            let t = format!("{} ", v["verdict"]["raw_text"].as_str().unwrap());
            v["verdict"]["raw_text"] = json!(t);
        }));
        assert!(e.contains("mirror-a") && e.contains("raw_text does not match"), "{e}");
        // a digest with no raw text
        let e = err(&edit("span-b-r0", 0, &|v: &mut Value| {
            v["verdict"].as_object_mut().unwrap().remove("raw_text");
        }));
        assert!(e.contains("span-b") && e.contains("no raw_text"), "{e}");
        // a first raw response (the stratum) that is not the digested one, on a failed item
        let e = err(&edit("screen-e-r0", 0, &|v: &mut Value| {
            v["verdict"]["raw_text"] = json!("Quiet Harbor Notes\nSomething else entirely.");
        }));
        assert!(e.contains("screen-e") && e.contains("raw_text does not match"), "{e}");
        // an item check that read another raw response
        let e = err(&edit("multi-c-r1", 0, &|v: &mut Value| {
            v["verdict"]["diag"]["post"]["edits"][0]["raw_response_sha256"] = json!(sha("another"));
        }));
        assert!(e.contains("multi-c") && e.contains("item check read other raw children"), "{e}");
        // one request with the digest and one without
        let e = err(&edit("multi-c-r0", 0, &|v: &mut Value| {
            v["verdict"]["diag"]["post"]["edits"][0].as_object_mut().unwrap().remove("raw_response_sha256");
        }));
        assert!(e.contains("multi-c") && e.contains("1 of 2 accepted requests"), "{e}");
        // a parent that is not the planned one: P2's one item has only a stratum note
        let mut ps2 = ps.clone();
        ps2.insert("p2".into(), format!("{} ", ps["p2"]));
        let e = run(&ps2, &cases, &st).err().map(|e| format!("{e:#}")).unwrap();
        assert!(e.contains("screen-e") && e.contains("parent_text_sha256"), "{e}");
        // P1's first item has a twin and no stratum, so the twin's own parent check names it
        let mut ps1 = ps.clone();
        ps1.insert("p1".into(), format!("{} ", ps["p1"]));
        let e = run(&ps1, &cases, &st).err().map(|e| format!("{e:#}")).unwrap();
        assert!(e.contains("mirror-a") && e.contains("parent_text_sha256"), "{e}");
        let e = format!("{:#}", raw_twin(&cases[0].item, &ps1["p1"], &st).unwrap_err());
        assert!(e.contains("mirror-a") && e.contains("parent_text_sha256"), "{e}");
        assert!(raw_twin(&cases[0].item, &ps["p1"], &st).unwrap().is_some());
        // the untouched journals finalize
        assert!(run(&ps, &cases, &st).is_ok());
    }

    /// A mirror of `parent` with one reply per attempt.
    fn mirror(id: &str, parent: &str, ptext: &str, replies: Vec<String>) -> Case {
        case(id, parent, ptext, vec![], vec![replies])
    }

    fn title(t: &str, body: String) -> String {
        format!("{t}\n{body}")
    }

    fn entry<'a>(m: &'a BTreeMap<String, Vec<Value>>, id: &str) -> &'a Value {
        m["train"].iter().find(|x| x["item_id"] == id).unwrap_or_else(|| panic!("no entry {id}"))
    }

    #[test]
    fn finalize_v2_first_raw_responses_skip_no_content_and_keep_pre_profile_rejections_out() {
        let ps = parents();
        let (p1, p2) = (&ps["p1"], &ps["p2"]);
        let mut cases = battery(p1, p2);
        // a nonempty reasoning block first (content, rejected before the profile step), then the form screen only
        cases.push(mirror("think-i", "p1", p1, vec![format!("<think>Plan the mirror first.</think>\n{}", prose(&ALT, 95, 17, 11)), title("A Quiet Fern Valley", prose(&ALT, 96, 17, 11))]));
        // an HTTP 500 first (no raw response), then the form screen only
        cases.push(mirror("http-j", "p1", p1, vec![HTTP_500_REPLY.into(), title("A Quiet Marsh Dawn", prose(&ALT, 97, 17, 11))]));
        // no content first (no raw response), then the form screen only
        cases.push(mirror("none-k", "p1", p1, vec![NO_CONTENT_REPLY.into(), title("A Quiet Dune Night", prose(&ALT, 98, 17, 11))]));
        let st = v2(&ps, &cases);
        let reasons = |rid: &str, a: u32| st[rid].responses[&a]["verdict"]["reasons"].clone();
        assert_eq!(reasons("think-i-r0", 0), json!(["nonempty_reasoning_in_content"]));
        assert_eq!(reasons("http-j-r0", 0), json!(["http_status_500", "no_content"]));
        assert_eq!(reasons("none-k-r0", 0), json!(["no_content"]));
        for rid in ["think-i-r0", "http-j-r0", "none-k-r0"] {
            assert_eq!(reasons(rid, 1), json!(["form_line_no_sentence_end"]), "{rid}");
            assert!(failed_form_screen_only(&st[rid].responses[&1]["verdict"]), "{rid}");
        }
        let o = run(&ps, &cases, &st).unwrap();
        // the reasoning block is the first raw response, and it was not the form screen's: no stratum
        let t = entry(&o.failures, "think-i");
        assert_eq!(t["status"], "terminal_failure");
        assert!(t.get(SCREEN_FAIL_ID).is_none(), "{}", t[SCREEN_FAIL_ID]);
        assert_eq!(first_raw_response(&st["think-i-r0"]).map(|x| x.0), Some(0));
        // after no content, attempt 1 is the first raw response
        for (id, rid) in [("http-j", "http-j-r0"), ("none-k", "none-k-r0")] {
            let d = &entry(&o.failures, id)[SCREEN_FAIL_ID];
            let raw = st[rid].responses[&1]["verdict"]["raw_text"].as_str().unwrap();
            assert_eq!(d["text"].as_str(), Some(raw), "{id}");
            assert_eq!(d["rform"]["raw_responses"], json!([{"request_id": rid, "attempt": 1, "raw_response_sha256": sha(raw), "accepted": false, "reasons": ["form_line_no_sentence_end"]}]));
            assert_eq!(screen_fail_doc(entry(&o.failures, id)), Some(d));
        }
        assert_eq!(o.summary["counts"]["train"][SCREEN_FAIL_ID], json!({"emitted": 4, "left_out:final_document_below_150_words": 1}));
    }

    #[test]
    fn finalize_v2_stratum_rides_with_an_item_excluded_in_finalize() {
        let ps = parents();
        let (p1, p2) = (&ps["p1"], &ps["p2"]);
        let mut cases = battery(p1, p2);
        // a title line first (the form screen only), then mirror-a's reply again: excluded by the generated-text dedup
        let again = cases[0].replies[0][0].clone();
        cases.push(mirror("dup-h", "p1", p1, vec![title("A Quiet Ridge Evening", prose(&ALT, 91, 17, 11)), again]));
        let st = v2(&ps, &cases);
        let o = run(&ps, &cases, &st).unwrap();
        let x = entry(&o.exclusions, "dup-h");
        assert!(x["reasons"].as_array().unwrap().iter().any(|r| r.as_str().unwrap().starts_with("generated_")), "{}", x["reasons"]);
        assert_eq!(o.summary["counts"]["train"]["status"]["excluded_in_finalize"], 1);
        // the exclusion keeps the item's stratum document, and no twin
        let d = &x[SCREEN_FAIL_ID];
        let raw = st["dup-h-r0"].responses[&0]["verdict"]["raw_text"].as_str().unwrap();
        assert_eq!((d["text"].as_str(), d["id"].as_str(), d["stratum"].as_str()), (Some(raw), Some("rform_screen_fail_v1:dup-h"), Some(SCREEN_FAIL_ID)));
        assert_eq!(screen_fail_doc(x), Some(d));
        assert!(x.get(RAW_TWIN_FIELD).is_none() && raw_twin_doc(x).is_none());
        assert_eq!(o.summary["counts"]["train"][SCREEN_FAIL_ID]["emitted"], 3);
    }

    #[test]
    fn finalize_v2_refuses_a_duplicate_rform_text() {
        let ps = parents();
        let (p1, p2) = (&ps["p1"], &ps["p2"]);
        let mut cases = battery(p1, p2);
        // a mirror of P2 whose first raw response is screen-d's: two stratum documents with one text
        let first = cases[3].replies[0][0].clone();
        cases.push(mirror("dup-s", "p2", p2, vec![first, prose(&ALT, 93, 18, 11)]));
        let st = v2(&ps, &cases);
        assert!(failed_form_screen_only(&st["dup-s-r0"].responses[&0]["verdict"]));
        let e = run(&ps, &cases, &st).err().map(|e| format!("{e:#}")).unwrap_or_default();
        assert!(e.contains("duplicate R-form text: rform_screen_fail_v1:dup-s has the text of rform_screen_fail_v1:screen-d"), "{e}");
        // with another first reply the run finalizes
        cases.last_mut().unwrap().replies[0][0] = title("A Quiet Pier Evening", prose(&ALT, 94, 17, 11));
        let st = v2(&ps, &cases);
        assert!(run(&ps, &cases, &st).is_ok());
    }

    /// Every `"form_…` string literal in `paths`, cut before the first byte
    /// that is not a lowercase letter, digit, `_` or `:` (so a `format!`
    /// code gives its prefix).
    fn form_literals(paths: &[std::path::PathBuf]) -> BTreeSet<String> {
        let mut out = BTreeSet::new();
        for p in paths {
            let s = std::fs::read_to_string(p).unwrap();
            for (i, _) in s.match_indices("\"form_") {
                let code: String = s[i + 1..].chars().take_while(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == '_' || *c == ':').collect();
                out.insert(code);
            }
        }
        out
    }

    /// Post-record keys that start with `form_` and are no reason.
    const FORM_RECORD_KEYS: [&str; 5] = ["form_reasons", "form_screen", "form_screen_applied", "form_screen_reading", "form_licence"];

    fn outside_screen(code: &str) -> bool {
        OUTSIDE_SCREEN_FORM_REASONS.iter().any(|c| if c.ends_with(':') { code.starts_with(c) } else { code == *c })
    }

    #[test]
    fn every_form_code_of_the_form_crate_and_the_wiring_is_classified() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let mut paths: Vec<std::path::PathBuf> = std::fs::read_dir(root.join("../grammar-a-form/src"))
            .unwrap()
            .map(|e| e.unwrap().path())
            .filter(|p| p.extension().is_some_and(|x| x == "rs"))
            .collect();
        paths.push(root.join("src/form_post.rs"));
        paths.sort();
        assert!(paths.iter().any(|p| p.ends_with("profile.rs")) && paths.iter().any(|p| p.ends_with("transfer.rs")));
        let codes: BTreeSet<String> = form_literals(&paths).into_iter().filter(|c| !FORM_RECORD_KEYS.contains(&c.as_str())).collect();
        // each code is a screen code or a listed code outside the screen, never both; a new
        // code fails here until it is classified (amendment 19 item 19.4)
        for c in &codes {
            assert!(is_screen_reason(c) != outside_screen(c), "unclassified or doubly classified form code {c:?}");
        }
        let screen: BTreeSet<&str> = codes.iter().map(String::as_str).filter(|c| is_screen_reason(c)).collect();
        assert_eq!(screen, BTreeSet::from(["form_frame_", "form_line_list_like", "form_line_no_sentence_end", "form_paragraph_"]));
        // every listed code outside the screen is still emitted
        for c in OUTSIDE_SCREEN_FORM_REASONS {
            assert!(codes.contains(c), "{c} is listed but no source emits it");
            assert!(!is_screen_reason(c));
        }
        // the paragraph and frame codes over every reason the licence screen can name
        for r in ["boilerplate_term", "removed_inline_content", "replacement_character", "control_character", "short_paragraph", "no_sentence_end", "url", "residual_markup", "non_ascii_letters", "long_quotation", "list_like_line"] {
            assert!(is_screen_reason(&format!("form_paragraph_{r}")) && is_screen_reason(&format!("form_frame_{r}")), "{r}");
        }
        assert!(!is_screen_reason("form_assembly_error:span order") && !is_screen_reason("form_future_check"));
    }

    #[test]
    fn fs_pairs_take_raw_twins_and_exclude_the_screen_fail_stratum() {
        let ps = parents();
        let cases = battery(&ps["p1"], &ps["p2"]);
        let st = v2(&ps, &cases);
        let o = run(&ps, &cases, &st).unwrap();
        let n = &o.records["train"];
        let r: Vec<&Value> = o.records.values().chain(o.failures.values()).chain(o.exclusions.values()).flatten().flat_map(rform_docs).collect();
        let pairs = fs_pairs(n.iter(), r.iter().copied());
        let want: Vec<(String, String)> = {
            let mut w: Vec<(String, String)> = n.iter().map(|x| (x["id"].as_str().unwrap().to_string(), format!("rform:{}", x["id"].as_str().unwrap()))).collect();
            w.sort();
            w
        };
        assert_eq!(pairs, want);
        assert!(pairs.iter().all(|(_, b)| !b.starts_with(SCREEN_FAIL_ID)));
        // a twin whose N-form record is not given is not paired, and the stratum never is
        let fewer: Vec<&Value> = n.iter().filter(|x| x["id"] != "span-b").collect();
        assert_eq!(fs_pairs(fewer.iter().copied(), r.iter().copied()).len(), 5);
        let strata: Vec<&Value> = r.iter().copied().filter(|d| d["stratum"] == SCREEN_FAIL_ID).collect();
        assert_eq!(strata.len(), 2);
        assert!(fs_pairs(n.iter(), strata.iter().copied()).is_empty());
    }

    /// A journal response line with a handmade verdict over `raw` (digest
    /// included unless `digest` is false).
    fn line(a: u32, accepted: bool, reasons: &[&str], form: &[&str], raw: &str, digest: bool) -> Value {
        let mut post = json!({"form_reasons": form});
        if digest {
            post["raw_response_sha256"] = json!(sha(raw));
        }
        json!({"kind": "response", "attempt": a, "verdict": {"accepted": accepted, "reasons": reasons, "text": raw, "raw_text": raw, "diag": {"post": post}}})
    }

    fn st_of(lines: Vec<Value>) -> ReqState {
        let mut s = ReqState::default();
        for l in lines {
            let a = l["attempt"].as_u64().unwrap() as u32;
            s.intents.push(a);
            s.responses.insert(a, l);
        }
        s
    }

    #[test]
    fn raw_digests_form_screen_reasons_and_first_raw_responses() {
        // the digest's places in a post record
        assert_eq!(raw_digest(&json!({"raw_response_sha256": "a"})), Some("a"));
        let item = |last: bool, index: u64, n: usize| {
            json!({"edits": (0..n).map(|k| json!({"raw_response_sha256": format!("e{k}")})).collect::<Vec<_>>(), "item_check": {"last": last, "index": index}})
        };
        assert_eq!(raw_digest(&item(true, 1, 2)), Some("e1"));
        assert_eq!(raw_digest(&item(false, 1, 1)), Some("e0"));
        assert_eq!(raw_digest(&json!({"edits": [{"raw_response_sha256": "s"}]})), Some("s"));
        assert_eq!(raw_digest(&json!({"edits": [{"raw_response_sha256": "s"}, {"raw_response_sha256": "t"}]})), None);
        assert_eq!(raw_digest(&json!({"input_sha256": "v1"})), None);
        assert_eq!(raw_digest(&Value::Null), None);
        // the form screen alone
        let v = |accepted: bool, reasons: &[&str], form: &[&str]| line(0, accepted, reasons, form, "x", true)["verdict"].clone();
        assert!(failed_form_screen_only(&v(false, &["form_line_list_like"], &["form_line_list_like"])));
        assert!(failed_form_screen_only(&v(false, &["form_frame_short_paragraph", "form_paragraph_url"], &["form_frame_short_paragraph", "form_paragraph_url"])));
        assert!(!failed_form_screen_only(&v(true, &[], &[])));
        assert!(!failed_form_screen_only(&v(false, &[], &[])));
        assert!(!failed_form_screen_only(&v(false, &["form_line_list_like", "final_document_below_150_words"], &["form_line_list_like"])));
        assert!(!failed_form_screen_only(&v(false, &["form_line_list_like", "echo_span_byte_identical"], &["form_line_list_like"])));
        assert!(!failed_form_screen_only(&v(false, &["form_line_list_like", "word_count_outside_0.5_1.5_of_target"], &["form_line_list_like"])));
        assert!(!failed_form_screen_only(&v(false, &["form_line_list_like"], &[])));
        // every form code outside the screen, a code added later included, keeps a verdict out
        for r in ["form_empty", "form_assembly_error:span order", "form_span_outside_parent", "form_donor_is_a_span", "form_parent_missing", "form_parent_not_in_prior", "form_future_check"] {
            assert!(!failed_form_screen_only(&v(false, &[r], &[r])), "{r}");
            assert!(!failed_form_screen_only(&v(false, &["form_line_list_like", r], &["form_line_list_like", r])), "{r}");
        }
        // the first raw response skips attempts that returned no model content
        let no_content = |a: u32, r: &str| {
            let mut l = line(a, false, &[r], &[], "", false);
            l["verdict"]["diag"] = json!({});
            l["verdict"].as_object_mut().unwrap().retain(|k, _| k != "text" && k != "raw_text");
            l
        };
        for r in ["http_status_500", "transport_error", "no_content"] {
            let s = st_of(vec![no_content(0, r), line(1, true, &[], &[], "Second.", true)]);
            assert_eq!(first_raw_response(&s).map(|x| x.0), Some(1), "{r}");
            assert!(!is_raw_response(&s.responses[&0]) && is_raw_response(&s.responses[&1]));
            assert!(first_raw_response(&st_of(vec![no_content(0, r)])).is_none(), "{r}");
        }
        // a response with content rejected before the profile step (no post record, no digest) is a raw response
        for r in ["nonempty_reasoning_in_content", "unclosed_reasoning_block", "no_sentence_end_within_budget"] {
            let mut pre = line(0, false, &[r], &[], "", false);
            pre["verdict"]["diag"] = json!({"finish_reason": "stop"});
            pre["verdict"].as_object_mut().unwrap().retain(|k, _| k != "text" && k != "raw_text");
            let s = st_of(vec![pre, line(1, false, &["form_line_list_like"], &["form_line_list_like"], "- A list.", true)]);
            assert_eq!(first_raw_response(&s).map(|x| x.0), Some(0), "{r}");
            assert!(!failed_form_screen_only(&s.responses[&0]["verdict"]), "{r}");
        }
        // empty content went through the profile step: a raw response, rejected outside the screen
        let empty = line(0, false, &["empty_content", "form_empty"], &["form_empty"], "", true);
        assert!(is_raw_response(&empty) && !failed_form_screen_only(&empty["verdict"]));
        // an intent with no response line is no raw response; the next response is
        let mut s = st_of(vec![line(1, false, &["form_line_list_like"], &["form_line_list_like"], "- A list.", true)]);
        s.intents.insert(0, 0);
        assert_eq!(first_raw_response(&s).map(|x| x.0), Some(1));
        // the raw child is read exactly, never trimmed
        let raw = "\n  An untrimmed raw child.  \n";
        assert_eq!(raw_child(&line(0, true, &[], &[], raw, true)["verdict"]).unwrap(), Some(raw));
        assert_eq!(raw_child(&line(0, true, &[], &[], raw, false)["verdict"]).unwrap(), None);
    }

    #[test]
    fn raw_twin_keeps_a_mirror_untrimmed_and_the_stratum_reads_every_request_of_an_item() {
        let p1 = parent_one();
        // a mirror's raw child is the whole twin, edge whitespace and all
        let m = case("mirror-x", "p1", &p1, vec![], vec![vec![String::new()]]);
        let raw = "\n  Lichen and fern covered the marsh wall.\n\nNobody minded at all.  \n";
        let st: HashMap<String, ReqState> = [("mirror-x-r0".to_string(), st_of(vec![line(0, true, &[], &[], raw, true)]))].into();
        let tw = raw_twin(&m.item, &p1, &st).unwrap().unwrap();
        assert_eq!((tw.text.as_str(), tw.ai_spans.clone(), tw.share), (raw, vec![(0, raw.len())], 1.0));
        // two requests: one first raw response accepted, the other failing the form screen only
        let (s2, s13) = (sentence(&p1, 2), sentence(&p1, 13));
        let it = case("multi-x", "p1", &p1, vec![s2, s13], vec![vec![String::new()], vec![String::new()]]).item;
        let (a, b) = ("Lichen and fern covered the old marsh wall near the ridge.", "- Acorn bramble canyon.\n- Dune estuary fern glacier heather.");
        let accepted_first = st_of(vec![line(0, true, &[], &[], a, true)]);
        let list_first = |second: bool| {
            let mut v = vec![line(0, false, &["form_line_list_like"], &["form_line_list_like"], b, true)];
            if second {
                v.push(line(1, true, &[], &[], "Heather and sorrel grew by the quiet ridge near the thicket today.", true));
            }
            st_of(v)
        };
        let st: HashMap<String, ReqState> = [("multi-x-r0".to_string(), accepted_first.clone()), ("multi-x-r1".to_string(), list_first(true))].into();
        match screen_fail(&it, &p1, &st).unwrap() {
            Some(ScreenFail::Doc(d)) => {
                assert_eq!(d.text, generation::assemble(&p1, &[s2, s13], &[a, b]).unwrap().0);
                assert_eq!((d.responses[0].attempt, d.responses[0].accepted, d.responses[1].attempt, d.responses[1].accepted), (0, true, 0, false));
            }
            other => panic!("{other:?}"),
        }
        // an echo among the rejections, a request with no raw response, or no rejection at all: no stratum
        let echo = st_of(vec![line(0, false, &["echo_span_byte_identical"], &[], a, true)]);
        let none = st_of(vec![line(0, false, &["no_content"], &[], "", false)]);
        let accepted_second = st_of(vec![line(0, true, &[], &[], "Heather and sorrel grew by the quiet ridge near the thicket today.", true)]);
        // a first raw response rejected before the profile step (a reasoning block), however its next attempt fared
        let think = |next: Value| {
            let mut pre = line(0, false, &["nonempty_reasoning_in_content"], &[], "", false);
            pre["verdict"]["diag"] = json!({"finish_reason": "stop"});
            pre["verdict"].as_object_mut().unwrap().retain(|k, _| k != "text" && k != "raw_text");
            st_of(vec![pre, next])
        };
        let think_list = think(line(1, false, &["form_line_list_like"], &["form_line_list_like"], b, true));
        for (r0, r1) in [(list_first(false), echo), (list_first(false), none), (accepted_first.clone(), accepted_second), (accepted_first.clone(), think_list)] {
            let st: HashMap<String, ReqState> = [("multi-x-r0".to_string(), r0), ("multi-x-r1".to_string(), r1)].into();
            assert!(screen_fail(&it, &p1, &st).unwrap().is_none());
        }
        // no content first: the request's first raw response is attempt 1
        let mut nc = line(0, false, &["no_content"], &[], "", false);
        nc["verdict"].as_object_mut().unwrap().retain(|k, _| k != "text" && k != "raw_text");
        let late = st_of(vec![nc, line(1, false, &["form_line_list_like"], &["form_line_list_like"], b, true)]);
        let st: HashMap<String, ReqState> = [("multi-x-r0".to_string(), accepted_first.clone()), ("multi-x-r1".to_string(), late)].into();
        match screen_fail(&it, &p1, &st).unwrap() {
            Some(ScreenFail::Doc(d)) => {
                assert_eq!(d.text, generation::assemble(&p1, &[s2, s13], &[a, b]).unwrap().0);
                assert_eq!((d.responses[0].attempt, d.responses[1].attempt, d.responses[1].accepted), (0, 1, false));
            }
            other => panic!("{other:?}"),
        }
        // a raw response that cannot be spliced is left out with the assembly error
        let empty = st_of(vec![line(0, false, &["form_line_list_like"], &["form_line_list_like"], "", true)]);
        let st: HashMap<String, ReqState> = [("multi-x-r0".to_string(), accepted_first), ("multi-x-r1".to_string(), empty)].into();
        match screen_fail(&it, &p1, &st).unwrap() {
            Some(ScreenFail::LeftOut { reasons, words, .. }) => {
                assert_eq!(words, None);
                assert!(reasons.len() == 1 && reasons[0].starts_with("assembly_error:"), "{reasons:?}");
            }
            other => panic!("{other:?}"),
        }
    }
}
