//! `renorm`: profile v2 re-verdict of existing A positives with no model
//! call (B4; protocol A2 §4.3, §6.2 and §7.2, as amended by amendments 4, 7,
//! 10, 13, 14, 15 and 20).
//!
//! The renorm path holds no HTTP client: it reads journals, plans, rows and
//! parents from disk, applies `grammar-a-form`'s rules and writes new files.
//! Every echo verdict comes from `grammar_a_form::echo::screen_item`, through
//! `form_post::FormPost` (`profile::child_v2`, `profile::item_v2`) or
//! `profile::child_v2` directly. Records hold ids, counts, outcomes and
//! digests; the re-verdict journals and the normalised rows hold text and
//! belong in ignored private storage.
//!
//! **Splits** (§2.4; amendment 20; `SPLIT_RULE`). Old Dev-select and the
//! consumed Test are never parsed. `dev_select` and `test` are refused as a
//! batch's split. Every line of a plan's items, requests and input, of a
//! journal, of a rows or donors file is selected by the split its split key
//! names, read from the line's bytes before it is parsed (amendment 15 item
//! 15.4's reading, `key_values`); lines of other splits are skipped unparsed
//! and counted. Whole files are hashed to check their recorded digests.
//!
//! **A journals** (`run_batch`): the journals of a local plan
//! (`slop_ninja_a_positives generate`; one split is one batch,
//! `local_batch`) or of a spliced plan (`slop_ninja_a_spliced generate`; one
//! plan population is one batch, `spliced_batch`).
//! * The batch's profile step is the `FormPost` that `generate --profile`
//!   (or the spliced generator's) would build for it: the prior of the
//!   parents of the batch's items under the run population's seed, keyed by
//!   the item id. The binding and prior record it writes
//!   (`form_post::write_binding`) are the ones a later profile v2 run of the
//!   batch checks, so the output runs directory resumes under `generate
//!   --profile` (D4) with the same record.
//! * Every request's record response, its earliest accepted response
//!   (`MULTIPLE_ACCEPTED_RULE`), is re-verdicted as if profile v2 had been
//!   in the acceptance path; any later accepted response of the request is
//!   rewritten as rejected. Its raw child, never trimmed, is the verdict's
//!   `raw_text` when profile v2's step ran (digest-checked against the post
//!   record's `raw_response_sha256`), else, when no post step ran, the
//!   verdict's `text`, the cleaned response a profile-off run accepted. A
//!   post-processed verdict without `raw_text` (another step's, such as a v1
//!   normaliser's) is refused, since its raw child cannot be recovered
//!   (`raw_child`). A re-verdict keeps `raw_text` only when its step is
//!   profile v2's, as `generation::verdict` does. The journal's `raw_body`
//!   is re-cleaned as a check and the outcome counted (`raw_body_check`).
//!   * Local plans (`reverdict_local`): `FormPost`'s step on the raw child,
//!     then `generation::verdict`'s checks on the laid-out text, in its
//!     order, except the overlap screen (the protected index is not read;
//!     the original verdict's overlap outcome stands) and the
//!     final-document window (the profile step applies it, as under
//!     `generate --profile`). A test pins this to `generation::verdict`.
//!   * Spliced plans (`reverdict_spliced`): the original verdict with
//!     `FormPost`'s reasons, laid-out text and raw text merged in, as
//!     `form_post::spliced_post` does. A test pins this to it.
//!   * An item with several requests is judged as `generate --profile`
//!     judges it, in item order: each request's step sees the item's earlier
//!     requests that passed as accepted (`FormPost::with_items`), so the
//!     last one judged with every sibling accepted gets the item check
//!     (`profile::item_v2` on the final document and its window) and earlier
//!     ones their own edit's rules, each laid out in its item with the raw
//!     texts of the requests before it that passed
//!     (`FormPost::set_layout_context`; amendment 29 item 29.1).
//! * **Records** (`reverdict-<batch>.jsonl`, no text): per item and request,
//!   the outcome and the full text-free profile record (`profile_record`:
//!   `transfer`, `form_reasons`, `unlicensed_reasons`, `would_be_unlicensed`,
//!   `remeasure`, `echo`, ...), the fields amendment 14 item 14.1's dry run
//!   counts.
//! * **Regeneration plan** (keyed by request id; `ATTEMPT_RULE`): one row per
//!   rejected request of an item of R0's records (`--records`; every item
//!   when none is given) whose every request was accepted in the journals.
//!   Attempts are counted per request from the first (r2 F19g):
//!   `attempts_used` is one more than the highest journaled attempt, and
//!   `attempts_remaining` is the request's limit less that
//!   (`Request::attempts`: its `attempt_limit` when it carries one, B5b's
//!   regenerated mirrors; else `generation::ATTEMPTS`, R0's two for every
//!   planned request). An item with a
//!   rejected request that has no attempt left is dropped with a receipt
//!   (`drop_out_of_attempts`; refilled by §6.4's rules); a mirror is
//!   regenerated through B5b's new request with `mirror_target_words_v2`
//!   (`regenerate_mirror`); any other item resumes (`regenerate`).
//! * **Output journals** (`<out>/<generator>/journal-renorm.jsonl`): every
//!   journal line of the batch's requests, in input order and byte for byte,
//!   except each re-verdicted response, whose `verdict` is replaced and which
//!   gains a `renorm` record, and the superseded accepted responses. Intents
//!   carry over, so a rejected request resumes with its remaining attempts.
//!   A local batch writes `regenerate-order-<generator>.txt`, the
//!   `generate --request-order` lists of the `regenerate` items, so that a D4
//!   resume runs only them (`RESUME_RULE`).
//!
//! **Rows** (`renorm_rows`): Train rows of a bundle build that no journal
//! regenerates (RAID, stage-3 legacy and hosted captures), normalised under
//! the profile and dropped with a receipt when they fail (§6.2):
//! * RAID rows (an external-pool cohort `raid-v1-*` with model text) take
//!   their RAID human (`RAID_HUMAN_RULE`): the one unattacked (`attack`
//!   `none`) human document of their source group in the RAID pool
//!   documents the assembler read, the bundle's row when it holds it.
//! * Stage-3 legacy rows and hosted captures (`STAGE3_PARENT_RULE`) take a
//!   recorded parent that is a human row of the rows; a stage-3 legacy row
//!   with no recorded parent and one span takes the stage-3 legacy human of
//!   its source group whose bytes equal it outside the span; a row whose
//!   model text is all of it, with no recorded parent, takes a layout donor
//!   (`pick_donor`, `DONOR_RULE`): the same-domain Train-2 human with the
//!   smallest plan key (`donor_key`: V21's form with x = `layout_donor` and
//!   d.id = row id `:` human id, no A6 key), whose layout properties only
//!   are transferred (`profile::donor_child_v2`). Its conventions are keyed
//!   draws from the prior. Other rows are dropped.
//! * The rows batch's prior holds its parents only (item 7.8): RAID humans
//!   and recorded or recovered parents, each in its own domain, under the
//!   population's seed; donors are not parents. Draws are keyed by the row
//!   id. The window runs on every laid-out document.
//! * `rows-normalised.jsonl` replaces, by bundle row id, the text, spans and
//!   share of the rows it lists (`ROWS_CONSUMER`).
//!
//! Every reading above that the protocol left open is fixed by amendment
//! 20 and named in the summaries' `rules` and `readings`.
use crate::form_post::{self, FormPost, ItemSpec};
use crate::generation::{self, PostProcess, ReqState};
use crate::plan::{self, Request};
use crate::{diverse, overlap, span_ops, splice, text};
use anyhow::{Context, Result, bail, ensure};
use grammar_a_form::conventions::{Counts, counts};
use grammar_a_form::profile::{self as fprofile, NORM_ID, PROFILE_V2, PriorV2, is_r2_population, norm_prior_seed, resolve_v2};
use grammar_a_form::transfer::SourceLayout;
use grammar_a_form::window::{LENGTH_WINDOW, LENGTH_WINDOW_ID};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use tokenizers::Tokenizer;

/// The summary of an A-journal re-verdict (`RENORM-<batch>.json`).
pub const RENORM_SCHEMA: &str = "slop_ninja_a_positives_renorm_v1";
/// One item's re-verdict record (`reverdict-<batch>.jsonl`; no text).
pub const REVERDICT_SCHEMA: &str = "slop_ninja_a_positives_reverdict_v1";
/// One row of the regeneration plan (`regeneration-plan-<batch>.jsonl`).
pub const REGEN_SCHEMA: &str = "slop_ninja_a_positives_regeneration_plan_v1";
/// The `renorm` record a re-verdicted journal response gains.
pub const RESPONSE_RENORM_SCHEMA: &str = "slop_ninja_a_positives_renorm_response_v1";
/// The summary of a rows re-verdict (`ROWS-RENORM.json`).
pub const ROWS_RENORM_SCHEMA: &str = "slop_ninja_a_rows_renorm_v1";
/// One row's re-verdict record (`rows-reverdict.jsonl`; no text).
pub const ROWS_REVERDICT_SCHEMA: &str = "slop_ninja_a_rows_reverdict_v1";
/// One normalised row (`rows-normalised.jsonl`; text).
pub const ROWS_NORMALISED_SCHEMA: &str = "slop_ninja_a_rows_normalised_v1";
/// The rows batch's prior record.
pub const ROWS_PRIOR_SCHEMA: &str = "slop_ninja_a_rows_form_prior_v2";
/// V21's plan key name of the layout donor draw.
pub const LAYOUT_DONOR_KEY: &str = "layout_donor";
/// The re-verdict journal of each generator directory.
pub const JOURNAL_NAME: &str = "journal-renorm.jsonl";
/// The RAID cohort family whose rows take their RAID human.
pub const RAID_COHORT_PREFIX: &str = "raid-v1-";
/// What the renorm path holds instead of an HTTP client.
pub const NO_MODEL_CALL: &str = "none: the renorm path holds no HTTP client, starts no process and makes no model, detector or network call";

/// The checks of `generation::verdict` that the local re-verdict re-runs on
/// the laid-out text, and those it leaves.
pub const LOCAL_RULE: &str = "FormPost's step on the raw child, then generation::verdict's checks on the laid-out text in its order (empty content, chat-template special tokens, words within 0.5-1.5 of the target, the base continuation's 20 words, byte identity with the replaced span, the span operation screens, the 12-word copy window, the single-span assembly's A-token limit), then the step's reasons; the overlap screen is not re-run (the protected index is not read; the original verdict's overlap outcome stands) and the final-document window is the step's";
/// The spliced re-verdict.
pub const SPLICED_RULE: &str = "the original splice verdict (its raw-response checks) with FormPost's reasons, laid-out text and raw text merged in, as form_post::spliced_post does";
/// The item rule.
pub const ITEM_RULE: &str = "an item's requests are re-verdicted in item order; each sees the item's earlier requests that passed as accepted (FormPost::with_items), so the last one judged with every sibling accepted gets the item check and earlier ones their own edit's rules (form_post::ITEM_CHECK_RULE), each earlier one laid out in its item with the raw texts of the requests before it that passed (amendment 29 item 29.1)";
/// The attempt rule of the regeneration plan.
pub const ATTEMPT_RULE: &str = "attempts are counted per request from the first (r2 F19g; amendment 20): attempts_used is one more than the highest journaled attempt (intent or response) and attempts_remaining is the request's limit less that (Request::attempts: its attempt_limit when it carries one, B5b's regenerated mirrors; else generation::ATTEMPTS = 2, R0's limit for every planned request), reported as attempt_limit; an item with a rejected request that has no attempt left is dropped with a receipt (drop_out_of_attempts; refilled by section 6.4's rules); otherwise a mirror's rejected request is regenerated through B5b's new request with mirror_target_words_v2 and the remaining attempts (regenerate_mirror), and any other item's rejected requests resume under generate --profile (regenerate)";

fn sha(b: &[u8]) -> String {
    text::sha_hex(b)
}

fn file_sha(p: &Path) -> Result<String> {
    Ok(sha(&fs::read(p).with_context(|| p.display().to_string())?))
}

fn read_json(p: &Path) -> Result<Value> {
    serde_json::from_slice(&fs::read(p).with_context(|| p.display().to_string())?).with_context(|| p.display().to_string())
}

fn jsonl_bytes(rows: &[Value]) -> Result<Vec<u8>> {
    let mut b = Vec::new();
    for r in rows {
        serde_json::to_writer(&mut b, r)?;
        b.push(b'\n');
    }
    Ok(b)
}

fn pretty(v: &Value) -> Result<Vec<u8>> {
    let mut b = serde_json::to_vec_pretty(v)?;
    b.push(b'\n');
    Ok(b)
}

fn strs(v: &Value) -> Vec<String> {
    v.as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default()
}

/// A new output directory (mode 700); refuses an existing path.
fn new_dir(out: &Path) -> Result<()> {
    ensure!(!out.exists(), "{} exists: renorm writes a new directory", out.display());
    fs::create_dir_all(out)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(out, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

// ------------------------------------------------------------------ splits read from bytes

/// The splits whose rows renorm never parses, in either spelling: old
/// Dev-select (§2.4: never read, scored or parsed) and the consumed Test
/// (§2.3).
pub const REFUSED_SPLITS: [&str; 2] = ["dev_select", "test"];

/// The §2.4 reading of the renorm path (amendment 20).
pub const SPLIT_RULE: &str = "every line of a plan, journal, rows or donors file is selected by the split its split key names, read from the line's bytes before it is parsed (amendment 15 item 15.4's reading), and a journal line also by its request_id read from its bytes (one of the batch's requests); lines of other splits, and journal lines naming dev_select or test, are skipped unparsed and counted; dev_select and test are refused as a batch's split; whole files are hashed to check their recorded digests";

/// Whether `s` names a refused split (`dev_select`, `dev-select`, `test`).
pub fn refused_split(s: &str) -> bool {
    REFUSED_SPLITS.contains(&s.replace('-', "_").as_str())
}

/// Every value of key `key` that a JSON line names, read from its bytes
/// without parsing the line (amendment 15 item 15.4's reading, as B10's
/// `line_splits`): the string after `"key"`, optional whitespace, `:` and
/// optional whitespace, up to the next quote. `None` for a value that is not
/// a string or holds an escape. In JSON a quote inside a string is escaped,
/// so these are keys of the line's objects.
pub fn key_values<'a>(line: &'a [u8], key: &str) -> Vec<Option<&'a [u8]>> {
    let k = format!("\"{key}\"");
    let k = k.as_bytes();
    let mut out = Vec::new();
    let mut at = 0;
    while let Some(p) = line[at..].windows(k.len()).position(|w| w == k) {
        let mut i = at + p + k.len();
        at = at + p + 1;
        while i < line.len() && line[i].is_ascii_whitespace() {
            i += 1;
        }
        if line.get(i) != Some(&b':') {
            continue; // a string value, not a key
        }
        i += 1;
        while i < line.len() && line[i].is_ascii_whitespace() {
            i += 1;
        }
        if line.get(i) != Some(&b'"') {
            out.push(None);
            continue;
        }
        let s = i + 1;
        match line[s..].iter().position(|&b| b == b'"' || b == b'\\') {
            Some(e) if line[s + e] == b'"' => out.push(Some(&line[s..s + e])),
            _ => out.push(None),
        }
    }
    out
}

/// A line's split as its `key` values say, read from its bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LineSplit {
    /// Every value names this split.
    One(String),
    /// The line names no such key.
    Missing,
    /// A value that cannot be read from the bytes, or values that differ.
    Unreadable,
}

/// The split that every `key` value of `line` names ([`key_values`]).
pub fn line_split(line: &[u8], key: &str) -> LineSplit {
    let mut one: Option<&[u8]> = None;
    let vs = key_values(line, key);
    if vs.is_empty() {
        return LineSplit::Missing;
    }
    for v in vs {
        match (v, one) {
            (None, _) => return LineSplit::Unreadable,
            (Some(x), None) => one = Some(x),
            (Some(x), Some(o)) if x == o => {}
            _ => return LineSplit::Unreadable,
        }
    }
    match std::str::from_utf8(one.unwrap_or_default()) {
        Ok(s) => LineSplit::One(s.to_string()),
        Err(_) => LineSplit::Unreadable,
    }
}

/// Lines skipped before parsing, by input and split.
pub type Skipped = BTreeMap<String, BTreeMap<String, usize>>;

/// The lines of JSONL file `p` whose split (`key`, read from the line's
/// bytes) `keep` takes, parsed; every other line is skipped before it is
/// parsed and counted in `skipped[input]` by its split. A line whose split
/// cannot be read from its bytes refuses the file, by line number only:
/// nothing of it is decoded.
fn read_split_lines(p: &Path, key: &str, keep: &dyn Fn(&str) -> bool, input: &str, skipped: &mut Skipped) -> Result<Vec<Value>> {
    let f = BufReader::new(fs::File::open(p).with_context(|| p.display().to_string())?);
    let mut v = Vec::new();
    for (n, l) in f.split(b'\n').enumerate() {
        let l = l?;
        if l.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        match line_split(&l, key) {
            LineSplit::One(s) if keep(&s) => v.push(serde_json::from_slice(&l).with_context(|| format!("{} line {}", p.display(), n + 1))?),
            LineSplit::One(s) => *skipped.entry(input.to_string()).or_default().entry(s).or_default() += 1,
            _ => bail!("{} line {}: no {key} that can be read from the line's bytes; refused before any parse", p.display(), n + 1),
        }
    }
    Ok(v)
}

// ------------------------------------------------------------------ batches

/// The kind of plan whose journals a batch reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlanKind {
    /// `slop_ninja_a_positives generate` journals (`generation::verdict`).
    Local,
    /// `slop_ninja_a_spliced generate` journals (`splice::verdict`).
    Spliced,
}

impl PlanKind {
    fn id(self) -> &'static str {
        match self {
            PlanKind::Local => "local",
            PlanKind::Spliced => "spliced",
        }
    }
}

/// One item of a batch, in plan order.
#[derive(Clone, Debug)]
pub struct RItem {
    pub item_id: String,
    pub parent_id: String,
    /// Local: the item kind (mirror, span, ...); spliced: the policy.
    pub kind: String,
    pub cohort: String,
    pub domain: String,
    /// The item's request ids, in item order.
    pub requests: Vec<String>,
    /// The item's spans or edits, for an item with several requests.
    pub spec: Option<ItemSpec>,
}

/// One batch of journals to re-verdict: its profile step, requests, items
/// and parents.
pub struct Batch<'a> {
    pub kind: PlanKind,
    /// The binding's batch name (`split-<split>` or `population-<population>`).
    pub name: String,
    /// The batch's profile step, unbound (`with_edits` applied for a spliced
    /// batch).
    pub form: FormPost,
    /// The batch's requests, every generator.
    pub requests: HashMap<String, Request>,
    pub items: Vec<RItem>,
    pub parents: HashMap<String, String>,
    /// The A tokenizer (local batches).
    pub tokenizer: Option<&'a Tokenizer>,
    /// Input digests for the summary.
    pub inputs: Value,
    /// The split of every item of the batch: journal lines of other splits
    /// are skipped before they are parsed.
    pub split: String,
    /// Plan lines skipped before parsing, by input and split.
    pub skipped: Skipped,
    /// R0's records among the batch's items (`--records`): when given, the
    /// regeneration plan covers only them.
    pub records: Option<BTreeSet<String>>,
}

/// R0's records of a batch: one item id per line (`order::parse_order`'s
/// format). Ids that are no item of the batch are counted, not refused.
pub fn read_records(p: &Path) -> Result<(BTreeSet<String>, String)> {
    let b = fs::read(p).with_context(|| p.display().to_string())?;
    let ids = crate::order::parse_order(std::str::from_utf8(&b)?)?;
    Ok((ids.into_iter().collect(), sha(&b)))
}

/// A local plan's batch: the items of `split`, their requests (every
/// generator) and parents (the split's input), and the profile step
/// `generate --profile --profile-population population` builds for that
/// split: the prior of the parents of the split's items. `dev_select` and
/// `test` are refused before anything is read; every items, requests and
/// input line is selected by its split read from its bytes, so lines of
/// other splits are never parsed (`SPLIT_RULE`).
pub fn local_batch<'a>(pdir: &Path, split: &str, population: &str, tokenizer: &'a Tokenizer) -> Result<Batch<'a>> {
    ensure!(!refused_split(split), "split {split} is refused: renorm never reads, scores or parses old Dev-select or the consumed Test (protocol A2 section 2.4; amendment 20)");
    let meta = read_json(&pdir.join("plan.json"))?;
    ensure!(meta["schema"] != splice::PLAN_SCHEMA, "{} is a spliced plan: its batch is a plan population (--population)", pdir.display());
    let items_path = pdir.join("items.jsonl");
    let items_sha = file_sha(&items_path)?;
    ensure!(meta["items_sha256"] == items_sha.as_str(), "items.jsonl changed since planning");
    let keep = |s: &str| s == split;
    let mut skipped = Skipped::new();
    let mut requests: HashMap<String, Request> = HashMap::new();
    let mut request_files = BTreeMap::new();
    for (g, rec) in meta["request_files"].as_object().context("plan.json request_files")? {
        let p = pdir.join("requests").join(format!("{g}.jsonl"));
        let h = file_sha(&p)?;
        ensure!(rec["sha256"] == h.as_str(), "request file changed: {g}");
        request_files.insert(g.clone(), h);
        for v in read_split_lines(&p, "split", &keep, "requests", &mut skipped)? {
            let r: Request = serde_json::from_value(v)?;
            ensure!(r.split == split, "request {}: split", r.request_id);
            ensure!(requests.insert(r.request_id.clone(), r).is_none(), "duplicate request id");
        }
    }
    let mut items: Vec<RItem> = Vec::new();
    let mut parent_ids: BTreeSet<String> = BTreeSet::new();
    for v in read_split_lines(&items_path, "split", &keep, "items", &mut skipped)? {
        let it: plan::Item = serde_json::from_value(v)?;
        ensure!(it.split == split, "item {}: split", it.item_id);
        for rid in &it.requests {
            ensure!(requests.get(rid).is_some_and(|r| r.item_id == it.item_id), "item {}: request {rid} is not in the plan's {split} requests", it.item_id);
        }
        let spec = if it.requests.len() > 1 {
            ensure!(it.kind != "mirror" && it.source_spans.len() == it.requests.len(), "item {}: {} requests, {} source spans", it.item_id, it.requests.len(), it.source_spans.len());
            let mut parts = Vec::new();
            for (rid, sp) in it.requests.iter().zip(&it.source_spans) {
                ensure!(requests[rid].source_span == Some(*sp), "item {}: request {rid} is not its span {sp:?}", it.item_id);
                parts.push((rid.clone(), *sp, None));
            }
            Some(ItemSpec { item_id: it.item_id.clone(), parts })
        } else {
            None
        };
        parent_ids.insert(it.parent_id.clone());
        items.push(RItem { item_id: it.item_id, parent_id: it.parent_id, kind: it.kind, cohort: it.cohort, domain: it.domain, requests: it.requests, spec });
    }
    ensure!(!items.is_empty(), "the plan has no {split} item");
    let inp = &meta["inputs"][split];
    ensure!(!inp.is_null() && span_ops::plan_reads_input(meta["schema"].as_str(), split), "the plan reads no {split} input");
    let ip = PathBuf::from(inp["path"].as_str().context("input path")?);
    let input_sha = file_sha(&ip)?;
    ensure!(inp["sha256"] == input_sha.as_str(), "input {split} changed since planning");
    let mut humans: Vec<plan::Human> = Vec::new();
    for row in read_split_lines(&ip, "frozen_split", &keep, "input", &mut skipped)? {
        let h = plan::human_from(&row, split)?;
        if parent_ids.contains(&h.id) {
            humans.push(h);
        }
    }
    ensure!(humans.len() == parent_ids.len(), "split {split}: {} item parents, {} rows of its input match them", parent_ids.len(), humans.len());
    let form = FormPost::new(population, humans.iter().map(|h| (h.id.as_str(), h.domain, h.text.as_str())))?;
    let parents = humans.into_iter().map(|h| (h.id, h.text)).collect();
    let inputs = json!({"plan": pdir, "plan_sha256": file_sha(&pdir.join("plan.json"))?, "plan_schema": meta["schema"], "items_sha256": items_sha,
        "request_files": request_files, "split": split, "input": {"path": ip, "sha256": input_sha}});
    Ok(Batch { kind: PlanKind::Local, name: format!("split-{split}"), form, requests, items, parents, tokenizer: Some(tokenizer), inputs, split: split.to_string(), skipped, records: None })
}

/// A spliced plan's batch: the items of plan population `plan_population`,
/// their requests and parents, and the profile step the spliced generator
/// builds for it under `population`: the prior of the parents of every item
/// of that plan population, with the items' edits. Items, parents and
/// requests are selected by their split read from their bytes: lines naming
/// `dev_select` or `test` are skipped unparsed (spliced plans hold none).
pub fn spliced_batch(pdir: &Path, plan_population: &str, population: &str) -> Result<Batch<'static>> {
    spliced_batch_of(pdir, plan_population, population, &|s: &str| !refused_split(s))
}

/// `spliced_batch` reading only the items, parents and request lines whose
/// split (read from the line's bytes) `keep` takes; every other line is
/// skipped unparsed and counted in `Batch::skipped`. `spliced_batch` keeps
/// every split but `dev_select` and `test`; the dry run's counts (B23) keep
/// Train lines only (BUILD-PLAN 12.3). A plan population's items are of one
/// split, so a `keep` that takes it gives the same items, parents, requests
/// and prior.
pub fn spliced_batch_of(pdir: &Path, plan_population: &str, population: &str, keep: &dyn Fn(&str) -> bool) -> Result<Batch<'static>> {
    let meta = read_json(&pdir.join("plan.json"))?;
    ensure!(meta["schema"] == splice::PLAN_SCHEMA, "{} is not a spliced plan ({})", pdir.display(), splice::PLAN_SCHEMA);
    let items_sha = file_sha(&pdir.join("items.jsonl"))?;
    ensure!(meta["items"]["sha256"] == items_sha.as_str(), "items.jsonl changed since planning");
    let parents_sha = file_sha(&pdir.join("parents.jsonl"))?;
    ensure!(meta["parents"]["sha256"] == parents_sha.as_str(), "parents.jsonl changed since planning");
    let keep = |s: &str| !refused_split(s) && keep(s);
    let mut skipped = Skipped::new();
    let all: Vec<splice::Item> =
        read_split_lines(&pdir.join("items.jsonl"), "split", &keep, "items", &mut skipped)?.into_iter().map(serde_json::from_value).collect::<Result<_, _>>()?;
    let sitems: Vec<splice::Item> = all.into_iter().filter(|i| i.population == plan_population).collect();
    ensure!(!sitems.is_empty(), "the plan has no {plan_population} item");
    let split = sitems[0].split.clone();
    ensure!(sitems.iter().all(|i| i.split == split) && !refused_split(&split), "plan population {plan_population}: its items are not of one split");
    let mut ptext: HashMap<String, splice::Parent> = HashMap::new();
    for v in read_split_lines(&pdir.join("parents.jsonl"), "split", &keep, "parents", &mut skipped)? {
        let p: splice::Parent = serde_json::from_value(v)?;
        ensure!(sha(p.text.as_bytes()) == p.text_sha256, "parent digest {}", p.id);
        ptext.insert(p.id.clone(), p);
    }
    let ids: BTreeSet<&str> = sitems.iter().map(|i| i.parent_id.as_str()).collect();
    let mut rows: Vec<&splice::Parent> = Vec::new();
    for id in &ids {
        rows.push(ptext.get(*id).with_context(|| format!("parent {id} of a {plan_population} item"))?);
    }
    let mut edits: HashMap<String, splice::Edit> = HashMap::new();
    for it in &sitems {
        for e in &it.edits {
            ensure!(edits.insert(e.request_id.clone(), e.clone()).is_none(), "request {} is in two edits", e.request_id);
        }
    }
    let mut requests: HashMap<String, Request> = HashMap::new();
    let mut request_files = BTreeMap::new();
    for (g, rec) in meta["request_files"].as_object().context("plan.json request_files")? {
        let p = pdir.join("requests").join(format!("{g}.jsonl"));
        let h = file_sha(&p)?;
        ensure!(rec["sha256"] == h.as_str(), "request file changed: {g}");
        request_files.insert(g.clone(), h);
        for v in read_split_lines(&p, "split", &keep, "requests", &mut skipped)? {
            let r: Request = serde_json::from_value(v)?;
            if edits.contains_key(&r.request_id) {
                ensure!(r.split == split, "request {}: split", r.request_id);
                ensure!(requests.insert(r.request_id.clone(), r).is_none(), "duplicate request id");
            }
        }
    }
    let form = FormPost::new(population, rows.iter().map(|p| (p.id.as_str(), p.domain.as_str(), p.text.as_str())))?.with_edits(&edits);
    let mut items = Vec::new();
    for it in &sitems {
        let reqs: Vec<String> = it.edits.iter().map(|e| e.request_id.clone()).collect();
        for rid in &reqs {
            ensure!(requests.get(rid).is_some_and(|r| r.item_id == it.item_id), "item {}: request {rid} is not in the plan's request files", it.item_id);
        }
        let spec = (it.edits.len() > 1).then(|| ItemSpec {
            item_id: it.item_id.clone(),
            parts: it.edits.iter().map(|e| (e.request_id.clone(), e.parent_span, e.insert_sep.clone())).collect(),
        });
        items.push(RItem {
            item_id: it.item_id.clone(),
            parent_id: it.parent_id.clone(),
            kind: it.policy.clone(),
            cohort: splice::COHORT_ID.into(),
            domain: it.domain.clone(),
            requests: reqs,
            spec,
        });
    }
    let parents = rows.iter().map(|p| (p.id.clone(), p.text.clone())).collect();
    let inputs = json!({"plan": pdir, "plan_sha256": file_sha(&pdir.join("plan.json"))?, "plan_schema": meta["schema"], "items_sha256": items_sha,
        "parents_sha256": parents_sha, "request_files": request_files, "plan_population": plan_population});
    Ok(Batch { kind: PlanKind::Spliced, name: format!("population-{plan_population}"), form, requests, items, parents, tokenizer: None, inputs, split, skipped, records: None })
}

// ------------------------------------------------------------------ re-verdicts

/// The local re-verdict of one accepted response (`LOCAL_RULE`): `post`'s
/// step on the raw child `clean`, then `generation::verdict`'s checks on the
/// laid-out text. `orig` is the original verdict (its finish reason and raw
/// word count carry over). The result is the verdict `generate --profile`
/// gives the same response, the overlap screen aside.
pub fn reverdict_local(post: &dyn PostProcess, r: &Request, parent: Option<&str>, clean: &str, orig: &Value, tok: &Tokenizer) -> Value {
    let mut reasons: Vec<String> = Vec::new();
    let mut diag = json!({});
    diag["finish_reason"] = orig["diag"]["finish_reason"].clone();
    if r.endpoint == "completion" {
        diag["raw_words"] = orig["diag"]["raw_words"].clone();
    }
    let o = post.apply(r, parent, clean);
    diag["post"] = o.record;
    let t = o.text;
    if t.trim().is_empty() {
        reasons.push("empty_content".into());
    }
    let sp = text::special_tokens(&t);
    if !sp.is_empty() {
        reasons.push("chat_template_special_tokens".into());
        diag["special_markers"] = json!(sp);
    }
    let w = text::words(&t);
    diag["words"] = json!(w);
    let lo = (r.target_words as f64) * 0.5;
    let hi = (r.target_words as f64) * 1.5;
    if (w as f64) < lo || (w as f64) > hi {
        reasons.push("word_count_outside_0.5_1.5_of_target".into());
    }
    if r.template_id == plan::BASE_CONT_TEMPLATE_ID && w < 20 {
        reasons.push("continuation_below_20_words".into());
    }
    if let (Some((s, e)), Some(p)) = (r.source_span, parent) {
        match p.get(s..e) {
            Some(src) => {
                if t == src {
                    reasons.push("byte_identical_to_replaced_span".into());
                }
                let unseen_run = matches!(r.op.as_deref(), Some("continue" | "prefix_continue" | "middle_infill"));
                if r.op.is_some() && r.endpoint == "chat" && !unseen_run {
                    diag["span_word_change_fraction"] = json!(diverse::change_fraction(src, &t));
                }
                if let Some(op) = r.op.as_deref() {
                    reasons.extend(span_ops::screen(op, &t, p, (s, e), &mut diag));
                }
                if r.split != "test" && (r.endpoint == "completion" || r.op.as_deref() == Some("continue")) && overlap::shares_window(&t, src, 12) {
                    reasons.push("copies_removed_human_text_12_word_window".into());
                }
                diag["shares_parent_50_word_window"] = json!(overlap::shares_window(&t, p, 50));
            }
            None => reasons.push("assembly_error:span outside parent".into()),
        }
    }
    if let Some(p) = parent {
        let doc = match r.source_span {
            Some(span) => generation::assemble(p, &[span], &[t.as_str()]).map(|x| x.0),
            None => Ok(t.clone()),
        };
        match doc {
            Ok(d) => match generation::a_tokens(tok, &d) {
                Ok(n) => {
                    diag["a_tokens_single_span_assembly"] = json!(n);
                    if n > generation::MAX_A_TOKENS {
                        reasons.push("assembled_over_2048_a_tokens".into());
                    }
                }
                Err(e) => reasons.push(format!("tokenizer_error:{e}")),
            },
            Err(e) => reasons.push(format!("assembly_error:{e}")),
        }
    } else {
        reasons.push("parent_missing".into());
    }
    reasons.extend(o.reasons);
    let mut out = json!({"accepted": false});
    out["text"] = json!(t);
    if post.is_profile_v2() {
        out["raw_text"] = json!(clean);
    }
    out["accepted"] = json!(reasons.is_empty());
    out["reasons"] = json!(reasons);
    out["diag"] = diag;
    out
}

/// The spliced re-verdict of one accepted response (`SPLICED_RULE`): the
/// original verdict `orig` with `post`'s step on the raw child `raw` merged
/// in, as `form_post::spliced_post` merges it.
pub fn reverdict_spliced(post: &dyn PostProcess, r: &Request, parent: &str, raw: &str, orig: &Value) -> Value {
    let mut v = orig.clone();
    let o = post.apply(r, Some(parent), raw);
    let mut reasons = strs(&orig["reasons"]);
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
    v
}

/// The cleaned response the original verdict read from the journal's
/// `raw_body`, re-derived as `generation::verdict` (local) or
/// `splice::verdict` (spliced) derives it.
fn recleaned(kind: PlanKind, r: &Request, raw_body: &Value) -> Option<Option<String>> {
    let body = raw_body.as_str()?;
    let parsed: Value = serde_json::from_str(body).ok()?;
    Some(match kind {
        PlanKind::Local if r.endpoint == "completion" => {
            parsed["content"].as_str().and_then(|c| text::cut_to_budget(c, r.cut_budget.unwrap_or(r.target_words)))
        }
        PlanKind::Local => parsed["choices"][0]["message"]["content"].as_str().and_then(|c| text::strip_empty_think(c).ok()),
        PlanKind::Spliced => parsed["choices"][0]["message"]["content"].as_str().and_then(|c| text::strip_empty_think(c).ok().map(|t| t.trim().to_owned())),
    })
}

// ------------------------------------------------------------------ journals

/// An accepted journaled response: the record response of its request.
#[derive(Clone, Debug)]
struct Acc {
    attempt: u32,
    verdict: Value,
    raw: String,
    raw_body_check: &'static str,
    /// (journal file index, line index) of the response.
    at: (usize, usize),
}

#[derive(Clone, Debug, Default)]
struct RState {
    attempts: BTreeSet<u32>,
    acc: Option<Acc>,
}

impl RState {
    /// Attempts used, counted from the first: one more than the highest
    /// journaled attempt.
    fn used(&self) -> u32 {
        self.attempts.iter().next_back().map(|a| a + 1).unwrap_or(0)
    }
}

/// Journal lines read past without parsing, or unparsable.
#[derive(Debug, Default)]
struct LineCounts {
    /// Lines of other splits, skipped before parsing, by split.
    other_splits: BTreeMap<String, usize>,
    /// Lines with no split that can be read from their bytes (torn),
    /// skipped before parsing.
    unkeyed: usize,
    /// Lines of the batch's split that do not parse (torn).
    torn: usize,
}

struct Journals {
    /// (generator directory name, journal file), in sorted order.
    files: Vec<(String, PathBuf)>,
    states: HashMap<String, RState>,
    /// Lines of the batch's split whose request is not the batch's.
    lines_other: usize,
    counts: LineCounts,
    /// Accepted responses that are not their request's record response
    /// (`MULTIPLE_ACCEPTED_RULE`), by position.
    superseded: BTreeSet<(usize, usize)>,
    raw_body_checks: BTreeMap<&'static str, usize>,
}

/// A request's record response when its journals hold several accepted
/// responses, and what renorm writes for the others.
pub const MULTIPLE_ACCEPTED_RULE: &str = "a request's record response is its earliest accepted response (the lowest attempt, then the first in journal order), the one generation::ReqState::accepted and finalize read; only it is re-verdicted, and every other accepted response of the request is rewritten as rejected (renorm_superseded_accepted_response) with a renorm record, so the output journals, the regeneration plan and finalize agree";
/// The reason a superseded accepted response is rewritten with.
pub const SUPERSEDED_REASON: &str = "renorm_superseded_accepted_response";

/// Streams the journal lines of `p` that belong to `batch`, parsed, with
/// their line index and bytes: a line is parsed when the split read from
/// its bytes is not a refused one and either the `request_id` read from its
/// bytes is one of the batch's requests or its split is the batch's split.
/// Every other line is counted in `c` (when given) by its split, or as
/// unkeyed, and never parsed.
fn each_split_line(batch: &Batch, p: &Path, mut c: Option<&mut LineCounts>, mut f: impl FnMut(usize, &[u8], Value) -> Result<()>) -> Result<()> {
    for (n, l) in BufReader::new(fs::File::open(p)?).split(b'\n').enumerate() {
        let l = l?;
        if l.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        let split = line_split(&l, "split");
        let refused = matches!(&split, LineSplit::One(s) if refused_split(s));
        let ours = matches!(line_split(&l, "request_id"), LineSplit::One(r) if batch.requests.contains_key(&r));
        if !refused && (ours || split == LineSplit::One(batch.split.clone())) {
            match serde_json::from_slice::<Value>(&l) {
                Ok(v) => f(n, &l, v)?,
                Err(_) => {
                    if let Some(c) = c.as_deref_mut() {
                        c.torn += 1;
                    }
                }
            }
        } else if let Some(c) = c.as_deref_mut() {
            match split {
                LineSplit::One(s) => *c.other_splits.entry(s).or_default() += 1,
                _ => c.unkeyed += 1,
            }
        }
    }
    Ok(())
}

fn journal_set(runs: &Path) -> Result<Vec<(String, PathBuf)>> {
    let mut files = Vec::new();
    for d in splice::run_dirs(runs)? {
        let name = d.file_name().and_then(|n| n.to_str()).context("run directory name")?.to_string();
        for p in generation::journal_files(&d)? {
            files.push((name.clone(), p));
        }
    }
    Ok(files)
}

/// The raw-response digest a post record names for request `r`: the
/// record's own (`child_v2`), else that of its edit at `r`'s span
/// (`item_v2`'s per-edit records).
fn post_raw_digest<'v>(post: &'v Value, r: &Request) -> Option<&'v str> {
    if let Some(d) = post["raw_response_sha256"].as_str() {
        return Some(d);
    }
    let (s, e) = r.source_span?;
    post["edits"].as_array()?.iter().find(|x| x["span"] == json!([s, e])).and_then(|x| x["raw_response_sha256"].as_str())
}

/// The raw child of an accepted verdict: `raw_text` when profile v2's step
/// ran (checked against the post record's digest when it names one), else
/// `text` when no post step ran. A verdict of another post step (a v1
/// normaliser's) keeps no `raw_text`, so its raw child cannot be recovered
/// and it is refused.
fn raw_child(r: &Request, a: u32, verdict: &Value) -> Result<String> {
    let rid = &r.request_id;
    match verdict["raw_text"].as_str() {
        Some(t) => {
            if let Some(d) = post_raw_digest(&verdict["diag"]["post"], r) {
                ensure!(sha(t.as_bytes()) == d, "request {rid} attempt {a}: raw_text does not match the post record's raw_response_sha256");
            }
            Ok(t.to_string())
        }
        None => {
            ensure!(
                verdict["diag"].get("post").is_none_or(Value::is_null),
                "request {rid} attempt {a}: a post-processed verdict without raw_text; its raw child cannot be recovered"
            );
            Ok(verdict["text"].as_str().with_context(|| format!("request {rid} attempt {a}: accepted verdict without text"))?.to_string())
        }
    }
}

fn read_journals(batch: &Batch, runs: &Path) -> Result<Journals> {
    let files = journal_set(runs)?;
    let mut states: HashMap<String, RState> = HashMap::new();
    let mut lines_other = 0usize;
    let mut counts = LineCounts::default();
    let mut accepted_at: HashMap<String, Vec<(usize, usize)>> = HashMap::new();
    for (fi, (_, p)) in files.iter().enumerate() {
        each_split_line(batch, p, Some(&mut counts), |n, _, v| {
            let rid = v["request_id"].as_str().with_context(|| format!("{}: a journal line without request_id", p.display()))?;
            let Some(r) = batch.requests.get(rid) else {
                lines_other += 1;
                return Ok(());
            };
            let a = v["attempt"].as_u64().with_context(|| format!("{}: attempt", p.display()))? as u32;
            let st = states.entry(rid.to_string()).or_default();
            st.attempts.insert(a);
            match v["kind"].as_str() {
                Some("intent") => {}
                Some("response") => {
                    if v["verdict"]["accepted"] != true {
                        return Ok(());
                    }
                    accepted_at.entry(rid.to_string()).or_default().push((fi, n));
                    if st.acc.as_ref().is_some_and(|x| x.attempt <= a) {
                        return Ok(());
                    }
                    let raw = raw_child(r, a, &v["verdict"])?;
                    let check = match recleaned(batch.kind, r, &v["raw_body"]) {
                        None if v["raw_body"].is_null() => "absent",
                        None => "unparsable",
                        Some(Some(c)) if c == raw => "equal",
                        Some(_) => "differs",
                    };
                    st.acc = Some(Acc { attempt: a, verdict: v["verdict"].clone(), raw, raw_body_check: check, at: (fi, n) });
                }
                _ => bail!("{}: journal kind", p.display()),
            }
            Ok(())
        })?;
    }
    let mut superseded = BTreeSet::new();
    for (rid, ats) in &accepted_at {
        let chosen = states[rid].acc.as_ref().map(|a| a.at);
        superseded.extend(ats.iter().copied().filter(|at| Some(*at) != chosen));
    }
    let mut raw_body_checks = BTreeMap::new();
    for st in states.values() {
        if let Some(a) = &st.acc {
            *raw_body_checks.entry(a.raw_body_check).or_default() += 1;
        }
    }
    Ok(Journals { files, states, lines_other, counts, superseded, raw_body_checks })
}

// ------------------------------------------------------------------ judging

struct Judged {
    /// (request id, attempt) -> the new verdict.
    verdicts: HashMap<(String, u32), Value>,
    records: Vec<Value>,
    plan: Vec<Value>,
    counts: Value,
}

/// Re-verdicts one accepted response under `post`.
fn reverdict(batch: &Batch, post: &FormPost, r: &Request, parent: &str, acc: &Acc) -> Result<Value> {
    Ok(match batch.kind {
        PlanKind::Local => reverdict_local(post, r, Some(parent), &acc.raw, &acc.verdict, batch.tokenizer.context("a local batch needs the A tokenizer")?),
        PlanKind::Spliced => reverdict_spliced(post, r, parent, &acc.raw, &acc.verdict),
    })
}

/// An accepted verdict as a sibling's journal state (`FormPost::with_items`
/// reads its `text` and `raw_text`).
fn accepted_state(attempt: u32, verdict: &Value) -> ReqState {
    ReqState { intents: vec![attempt], responses: BTreeMap::from([(attempt, json!({"verdict": verdict}))]), limit: None }
}

/// Judges every item in plan order (`ITEM_RULE`), in two bindings that
/// give the sequential judgement exactly: (a) every accepted request with
/// its item's other requests pending, which is each request's verdict when
/// an earlier sibling of its item has not passed, and every earlier
/// request's; each is laid out in its item with the raw texts of the
/// requests before it (item order) that passed (a), as the sequential
/// judgement lays it out (amendment 29 item 29.1; the records keep (a)'s
/// pending siblings); (b) the last request of each item whose every request
/// was accepted in the journals and whose earlier requests passed (a), with
/// those siblings accepted, which is the item check.
fn judge(batch: &Batch, j: &Journals) -> Result<Judged> {
    let specs: Vec<ItemSpec> = batch.items.iter().filter_map(|it| it.spec.clone()).collect();
    let form_a = batch.form.clone().with_items(specs.clone(), &HashMap::new())?;
    let acc_of = |rid: &str| j.states.get(rid).and_then(|s| s.acc.as_ref());
    let parent_of = |it: &RItem| batch.parents.get(&it.parent_id).map(String::as_str).with_context(|| format!("item {}: parent {} missing", it.item_id, it.parent_id));
    let mut verdicts: HashMap<(String, u32), Value> = HashMap::new();
    for it in &batch.items {
        let parent = parent_of(it)?;
        for rid in &it.requests {
            if let Some(acc) = acc_of(rid) {
                let v = reverdict(batch, &form_a, &batch.requests[rid], parent, acc)?;
                if it.spec.is_some() && v["accepted"] == true {
                    form_a.set_layout_context(rid, v["raw_text"].as_str().unwrap_or(&acc.raw));
                }
                verdicts.insert((rid.clone(), acc.attempt), v);
            }
        }
        form_a.clear_layout_context();
    }
    let mut siblings: HashMap<String, ReqState> = HashMap::new();
    let mut last: Vec<(&RItem, &String)> = Vec::new();
    for it in batch.items.iter().filter(|it| it.spec.is_some()) {
        let (head, tail) = it.requests.split_at(it.requests.len() - 1);
        let complete = it.requests.iter().all(|rid| acc_of(rid).is_some());
        if complete && head.iter().all(|rid| verdicts[&(rid.clone(), acc_of(rid).unwrap().attempt)]["accepted"] == true) {
            for rid in head {
                let a = acc_of(rid).unwrap().attempt;
                siblings.insert(rid.clone(), accepted_state(a, &verdicts[&(rid.clone(), a)]));
            }
            last.push((it, &tail[0]));
        }
    }
    if !last.is_empty() {
        let form_b = batch.form.clone().with_items(specs, &siblings)?;
        for (it, rid) in last {
            let acc = acc_of(rid).unwrap();
            let v = reverdict(batch, &form_b, &batch.requests[rid], parent_of(it)?, acc)?;
            verdicts.insert((rid.clone(), acc.attempt), v);
        }
    }
    let mut records = Vec::new();
    let mut plan_rows: Vec<Value> = Vec::new();
    let mut n_complete = 0usize;
    let mut n_pass = 0usize;
    let mut n_partial = 0usize;
    let mut by_reason: BTreeMap<String, usize> = BTreeMap::new();
    let mut actions: BTreeMap<&'static str, usize> = BTreeMap::new();
    let mut changed_text = 0usize;
    let mut n_records = 0usize;
    for it in &batch.items {
        let complete = it.requests.iter().all(|rid| acc_of(rid).is_some());
        let r0 = batch.records.as_ref().is_none_or(|ids| ids.contains(&it.item_id));
        n_records += usize::from(batch.records.is_some() && r0);
        let mut reqs = Vec::new();
        let mut rejected: Vec<(&str, u32, Vec<String>)> = Vec::new();
        let mut item_reasons: BTreeSet<String> = BTreeSet::new();
        for rid in &it.requests {
            let r = &batch.requests[rid];
            let st = j.states.get(rid).cloned().unwrap_or_default();
            let Some(acc) = &st.acc else {
                reqs.push(json!({"request_id": rid, "generator": r.generator, "accepted_in_journals": false, "attempts_used": st.used()}));
                continue;
            };
            let new = &verdicts[&(rid.clone(), acc.attempt)];
            let ok = new["accepted"] == true;
            let reasons = strs(&new["reasons"]);
            let laid = new["text"].as_str().unwrap_or_default();
            if laid != acc.raw {
                changed_text += 1;
            }
            if !ok {
                item_reasons.extend(reasons.iter().cloned());
                rejected.push((rid.as_str(), st.used(), reasons.clone()));
            }
            let post_rec = &new["diag"]["post"];
            reqs.push(json!({"request_id": rid, "generator": r.generator, "accepted_in_journals": true, "attempt": acc.attempt, "attempts_used": st.used(),
                "target_words": r.target_words, "original_reasons": acc.verdict["reasons"], "accepted": ok, "reasons": reasons,
                "raw_response_sha256": sha(acc.raw.as_bytes()), "raw_words": text::words(&acc.raw), "text_sha256": sha(laid.as_bytes()), "words": text::words(laid),
                "raw_body_check": acc.raw_body_check, "form_reasons": post_rec["form_reasons"], "form_screen_applied": post_rec["form_screen_applied"],
                "echo_rejected": post_rec["echo"]["rejected"], "item_check_last": post_rec["item_check"]["last"], "length_window": post_rec["length_window"],
                "profile_record": post_rec}));
        }
        let pass = complete && rejected.is_empty();
        let mut action = Value::Null;
        if complete && r0 {
            n_complete += 1;
            if pass {
                n_pass += 1;
            } else {
                for x in &item_reasons {
                    *by_reason.entry(x.clone()).or_default() += 1;
                }
                // a request's limit is Request::attempts: ATTEMPTS for every R0 request
                let a = if rejected.iter().any(|(rid, used, _)| *used >= batch.requests[*rid].attempts()) {
                    "drop_out_of_attempts"
                } else if it.kind == "mirror" {
                    "regenerate_mirror"
                } else {
                    "regenerate"
                };
                *actions.entry(a).or_default() += 1;
                action = json!(a);
                for (rid, used, reasons) in &rejected {
                    let r = &batch.requests[*rid];
                    plan_rows.push(json!({"schema": REGEN_SCHEMA, "request_id": rid, "item_id": it.item_id, "parent_id": it.parent_id, "generator": r.generator,
                        "split": r.split, "kind": it.kind, "cohort": it.cohort, "domain": it.domain, "batch": batch.name, "reasons": reasons,
                        "item_reasons": item_reasons.iter().collect::<Vec<_>>(), "attempts_used": used, "attempt_limit": r.attempts(),
                        "attempts_remaining": r.attempts().saturating_sub(*used), "action": a, "target_words": r.target_words, "rule": ATTEMPT_RULE}));
                }
            }
        } else if r0 && reqs.iter().any(|x| x["accepted_in_journals"] == true) {
            n_partial += 1;
        }
        records.push(json!({"schema": REVERDICT_SCHEMA, "item_id": it.item_id, "parent_id": it.parent_id, "kind": it.kind, "cohort": it.cohort,
            "domain": it.domain, "batch": batch.name, "r0_record": r0, "complete_in_journals": complete, "pass": pass,
            "reasons": item_reasons.iter().collect::<Vec<_>>(), "action": action, "requests": reqs}));
    }
    plan_rows.sort_by(|a, b| a["request_id"].as_str().cmp(&b["request_id"].as_str()));
    for w in plan_rows.windows(2) {
        ensure!(w[0]["request_id"] != w[1]["request_id"], "request {} planned twice", w[0]["request_id"]);
    }
    let records_n = batch.records.as_ref().map(|ids| json!({"listed": ids.len(), "items_of_the_batch": n_records, "not_items_of_the_batch": ids.len() - n_records}));
    let counts = json!({"items": batch.items.len(), "r0_records": records_n, "items_complete_in_journals": n_complete, "items_pass": n_pass, "items_fail": n_complete - n_pass,
        "items_partial_in_journals": n_partial, "fail_items_by_reason": by_reason, "fail_items_by_action": actions, "requests_reverdicted": verdicts.len(),
        "reverdicted_with_changed_text": changed_text, "regeneration_plan_rows": plan_rows.len()});
    Ok(Judged { verdicts, records, plan: plan_rows, counts })
}

/// Creates `path` with `bytes` (mode 600), refusing an existing file.
fn write_new(path: &Path, bytes: &[u8]) -> Result<String> {
    if let Some(d) = path.parent().filter(|d| !d.exists()) {
        fs::create_dir_all(d)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(d, fs::Permissions::from_mode(0o700))?;
        }
    }
    let mut o = fs::OpenOptions::new();
    o.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600);
    }
    let mut f = o.open(path).with_context(|| path.display().to_string())?;
    f.write_all(bytes)?;
    f.sync_all()?;
    Ok(sha(bytes))
}

/// One output journal being written (mode 600, created new), with its
/// running digest and counts.
struct OutJournal {
    dir: String,
    f: std::io::BufWriter<fs::File>,
    h: Sha256,
    lines: usize,
    reverdicted: usize,
}

impl OutJournal {
    fn open(out: &Path, dir: &str) -> Result<OutJournal> {
        let d = out.join(dir);
        if !d.exists() {
            fs::create_dir_all(&d)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&d, fs::Permissions::from_mode(0o700))?;
            }
        }
        let p = d.join(JOURNAL_NAME);
        let mut o = fs::OpenOptions::new();
        o.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            o.mode(0o600);
        }
        let f = o.open(&p).with_context(|| p.display().to_string())?;
        Ok(OutJournal { dir: dir.to_string(), f: std::io::BufWriter::new(f), h: Sha256::new(), lines: 0, reverdicted: 0 })
    }

    fn line(&mut self, b: &[u8]) -> Result<()> {
        self.f.write_all(b)?;
        self.f.write_all(b"\n")?;
        self.h.update(b);
        self.h.update(b"\n");
        self.lines += 1;
        Ok(())
    }

    fn close(self) -> Result<Value> {
        let f = self.f.into_inner().map_err(|e| anyhow::anyhow!(e.to_string()))?;
        f.sync_all()?;
        Ok(json!({"path": format!("{}/{JOURNAL_NAME}", self.dir), "sha256": format!("{:x}", self.h.finalize()), "lines": self.lines, "reverdicted": self.reverdicted}))
    }
}

/// Writes the re-verdict journals, streaming: every line of the batch's
/// requests in input order, byte for byte, except the re-verdicted
/// responses and the superseded accepted ones (`MULTIPLE_ACCEPTED_RULE`).
/// Only lines of the batch's split are parsed. Journal files are read
/// grouped by generator directory, so each output journal is written in one
/// pass.
fn write_journals(batch: &Batch, j: &Journals, judged: &Judged, runs: &Path, out: &Path) -> Result<(Vec<Value>, usize)> {
    let rec_base = json!({"schema": RESPONSE_RENORM_SCHEMA, "profile": PROFILE_V2, "population": batch.form.population(),
        "prior_sha256": batch.form.prior_sha256(), "batch": batch.name, "plan_kind": batch.kind.id()});
    let mut files = Vec::new();
    let mut cur: Option<OutJournal> = None;
    let mut superseded = 0usize;
    for (fi, (dir, p)) in j.files.iter().enumerate() {
        let rel = p.strip_prefix(runs).unwrap_or(p).display().to_string();
        each_split_line(batch, p, None, |n, line, mut v| {
            let rid = v["request_id"].as_str().unwrap_or_default().to_string();
            if !batch.requests.contains_key(&rid) {
                return Ok(());
            }
            if cur.as_ref().is_some_and(|c| c.dir != *dir) {
                files.push(cur.take().unwrap().close()?);
            }
            if cur.is_none() {
                cur = Some(OutJournal::open(out, dir)?);
            }
            let w = cur.as_mut().unwrap();
            let a = v["attempt"].as_u64().unwrap_or(u64::MAX) as u32;
            let chosen = v["kind"] == "response" && j.states.get(&rid).and_then(|s| s.acc.as_ref()).is_some_and(|x| x.at == (fi, n));
            let mut rec = rec_base.clone();
            rec["source_journal"] = json!(rel);
            rec["source_line"] = json!(n + 1);
            match (chosen, judged.verdicts.get(&(rid, a))) {
                (true, Some(new)) => {
                    let old = std::mem::replace(&mut v["verdict"], new.clone());
                    rec["original_accepted"] = old["accepted"].clone();
                    rec["original_reasons"] = old["reasons"].clone();
                    rec["original_text_sha256"] = json!(old["text"].as_str().map(|t| sha(t.as_bytes())));
                    rec["raw_response_sha256"] = json!(new["raw_text"].as_str().map(|t| sha(t.as_bytes())));
                    v["renorm"] = rec;
                    w.line(&serde_json::to_vec(&v)?)?;
                    w.reverdicted += 1;
                }
                _ if j.superseded.contains(&(fi, n)) => {
                    let old = v["verdict"].clone();
                    v["verdict"]["accepted"] = json!(false);
                    v["verdict"]["reasons"] = json!([SUPERSEDED_REASON]);
                    rec["original_accepted"] = old["accepted"].clone();
                    rec["original_reasons"] = old["reasons"].clone();
                    rec["original_text_sha256"] = json!(old["text"].as_str().map(|t| sha(t.as_bytes())));
                    rec["rule"] = json!(MULTIPLE_ACCEPTED_RULE);
                    v["renorm"] = rec;
                    w.line(&serde_json::to_vec(&v)?)?;
                    superseded += 1;
                }
                _ => w.line(line)?,
            }
            Ok(())
        })?;
    }
    if let Some(c) = cur {
        files.push(c.close()?);
    }
    Ok((files, superseded))
}

/// Per-generator item lists of the `regenerate` rows (plan order), the
/// `generate --request-order` files a D4 resume of the output journals
/// takes, so that no other item becomes pending (`RESUME_RULE`).
fn order_files(batch: &Batch, plan: &[Value]) -> BTreeMap<String, Vec<String>> {
    let mut want: HashMap<&str, BTreeSet<&str>> = HashMap::new();
    for r in plan.iter().filter(|r| r["action"] == "regenerate") {
        want.entry(r["generator"].as_str().unwrap_or_default()).or_default().insert(r["item_id"].as_str().unwrap_or_default());
    }
    let mut out: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for it in &batch.items {
        for (g, ids) in &want {
            if ids.contains(it.item_id.as_str()) {
                out.entry(g.to_string()).or_default().push(it.item_id.clone());
            }
        }
    }
    out
}

/// How a D4 resume of a local batch's output runs.
pub const RESUME_RULE: &str = "D4 resumes only the regenerate rows: generate --profile --profile-population P --splits S --runs OUT --request-order OUT/regenerate-order-{GEN}.txt on the output journals (each listed item's rejected requests are pending with their remaining attempts, and no other item becomes pending); regenerate_mirror rows are regenerated through B5b's new requests with mirror_target_words_v2 (section 6.3) and the remaining attempts, not by resume; drop_out_of_attempts rows are refilled by section 6.4's rules";

/// Re-verdicts a batch's journals under `runs` into the new directory
/// `out` (see the module notes): the binding and prior record, the
/// re-verdict journals, `reverdict-<batch>.jsonl`,
/// `regeneration-plan-<batch>.jsonl`, the local batches' per-generator
/// `regenerate-order-<generator>.txt` and `RENORM-<batch>.json`. Returns the
/// summary. Outputs are deterministic for the same inputs.
/// Amendment 38 item 38.1's applications in one request's profile record,
/// counted once over an item: an item's last request is judged on the whole
/// item (`profile::item_v2`, every sibling's edit in its `edits`), so only
/// its own edit (`item_check.index`) is its count; every other record holds
/// its request's edit alone.
pub fn request_line_splits(rec: &Value) -> u64 {
    match (rec["item_check"]["last"].as_bool(), rec["item_check"]["index"].as_u64()) {
        (Some(true), Some(i)) => grammar_a_form::profile::edit_line_splits(rec, i as usize),
        _ => grammar_a_form::profile::document_line_splits(rec),
    }
}

pub fn run_batch(batch: &Batch, runs: &Path, out: &Path) -> Result<Value> {
    ensure!(is_r2_population(batch.form.population()), "population {} is not a round-2 population id", batch.form.population());
    ensure!(!refused_split(&batch.split), "split {} is refused (protocol A2 section 2.4; amendment 20)", batch.split);
    ensure!(runs.is_dir(), "{}: no runs directory", runs.display());
    let j = read_journals(batch, runs)?;
    let judged = judge(batch, &j)?;
    new_dir(out)?;
    let binding = form_post::binding_path(out, &batch.name);
    form_post::write_binding(&binding, out, &batch.form)?;
    let (journals, superseded) = write_journals(batch, &j, &judged, runs, out)?;
    ensure!(superseded == j.superseded.len(), "superseded accepted responses: {superseded} written, {} found", j.superseded.len());
    let rv = format!("reverdict-{}.jsonl", batch.name);
    let rg = format!("regeneration-plan-{}.jsonl", batch.name);
    let rv_sha = write_new(&out.join(&rv), &jsonl_bytes(&judged.records)?)?;
    let rg_sha = write_new(&out.join(&rg), &jsonl_bytes(&judged.plan)?)?;
    let mut orders = BTreeMap::new();
    if batch.kind == PlanKind::Local {
        for (g, ids) in order_files(batch, &judged.plan) {
            let name = format!("regenerate-order-{g}.txt");
            let b: String = ids.iter().map(|i| format!("{i}\n")).collect();
            orders.insert(g, json!({"path": name, "sha256": write_new(&out.join(&name), b.as_bytes())?, "items": ids.len()}));
        }
    }
    let mut input_journals = Vec::new();
    for (dir, p) in &j.files {
        input_journals.push(json!({"generator_dir": dir, "path": p.strip_prefix(runs).unwrap_or(p), "sha256": file_sha(p)?}));
    }
    let mut counts = judged.counts.clone();
    counts["journal_lines_outside_batch"] = json!(j.lines_other);
    counts["journal_lines_other_splits_skipped_unparsed"] = json!(j.counts.other_splits);
    counts["journal_lines_without_a_readable_split_skipped_unparsed"] = json!(j.counts.unkeyed);
    counts["torn_lines"] = json!(j.counts.torn);
    counts["superseded_accepted_responses"] = json!(superseded);
    counts["plan_lines_skipped_unparsed"] = json!(batch.skipped);
    counts["raw_body_check"] = json!(j.raw_body_checks);
    // amendment 38 item 38.1's applications, each counted once: a request's own edit
    counts["document_line_splits"] = json!(judged.records.iter().flat_map(|r| r["requests"].as_array().into_iter().flatten()).map(|q| request_line_splits(&q["profile_record"])).sum::<u64>());
    let summary = json!({"schema": RENORM_SCHEMA, "code": crate::CODE_ID, "plan_kind": batch.kind.id(), "batch": batch.name, "split": batch.split,
        "profile": batch.form.record(), "prior_sha256": batch.form.prior_sha256(), "model_calls": NO_MODEL_CALL,
        "inputs": batch.inputs, "runs": runs, "input_journals": input_journals,
        "counts": counts,
        "outputs": {"binding": binding.strip_prefix(out).unwrap_or(&binding), "prior": format!("form-prior-{}.json", batch.form.prior_sha256()),
            "journals": journals, "reverdict": {"path": rv, "sha256": rv_sha}, "regeneration_plan": {"path": rg, "sha256": rg_sha}, "request_orders": orders},
        "rules": {"reverdict": match batch.kind { PlanKind::Local => LOCAL_RULE, PlanKind::Spliced => SPLICED_RULE }, "items": ITEM_RULE,
            "attempts": ATTEMPT_RULE, "multiple_accepted": MULTIPLE_ACCEPTED_RULE, "splits": SPLIT_RULE, "resume": RESUME_RULE,
            "raw_child": "the accepted verdict's raw_text when a post step ran (digest-checked), else its text; never trimmed; raw_body re-cleaned as a check (raw_body_check)"},
        "readings": [
            "an item is in the regeneration plan when it is one of R0's records (--records; every item when none is given) and every request has an accepted response in the journals (complete_in_journals)",
            "the regeneration plan counts attempts per request from the first attempt (amendment 20) against the request's limit (Request::attempts: its attempt_limit when it carries one, B5b's regenerated mirrors; else generation::ATTEMPTS = 2, R0's limit for every planned request)",
            "the overlap screen is not re-run on laid-out text; finalize's dedup re-runs on the final texts"
        ]});
    write_new(&out.join(format!("RENORM-{}.json", batch.name)), &pretty(&summary)?)?;
    Ok(summary)
}

/// `run_batch` with the summary's tokenizer digest (local batches), and
/// R0's records of the batch when `records` names them.
pub fn run_local(pdir: &Path, split: &str, population: &str, tokenizer_path: &Path, records: Option<&Path>, runs: &Path, out: &Path) -> Result<Value> {
    let mut tok = Tokenizer::from_file(tokenizer_path).map_err(|e| anyhow::anyhow!(e.to_string()))?;
    tok.with_truncation(None).map_err(|e| anyhow::anyhow!(e.to_string()))?;
    tok.with_padding(None);
    let mut batch = local_batch(pdir, split, population, &tok)?;
    batch.inputs["tokenizer"] = json!({"path": tokenizer_path, "sha256": file_sha(tokenizer_path)?});
    if let Some(p) = records {
        let (ids, h) = read_records(p)?;
        batch.inputs["records"] = json!({"path": p, "sha256": h, "ids": ids.len()});
        batch.records = Some(ids);
    }
    run_batch(&batch, runs, out)
}

// ------------------------------------------------------------------ rows

/// A layout donor candidate: a Train-2 human.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Donor {
    pub id: String,
    pub domain: String,
    pub text: String,
}

/// V21's plan key of donor `donor_id` for row `row_id`
/// (`SHA-256("slop_ninja_a_comp_r2_plan_v1:layout_donor:" + row id + ":" +
/// donor id)`, lowercase hex).
pub fn donor_key(row_id: &str, donor_id: &str) -> String {
    span_ops::r2_key(LAYOUT_DONOR_KEY, &format!("{row_id}:{donor_id}"))
}

/// The layout donor of row `row_id` in `domain`: the candidate of that
/// domain with the smallest `donor_key` (hex order is the big-endian
/// integer order; ties go to the smaller id). None when the domain has no
/// candidate.
pub fn pick_donor<'a>(row_id: &str, domain: &str, pool: &'a [Donor]) -> Option<&'a Donor> {
    pool.iter().filter(|d| d.domain == domain).map(|d| (donor_key(row_id, &d.id), d)).min_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.id.cmp(&b.1.id))).map(|x| x.1)
}

/// How one row is normalised.
#[derive(Clone, Debug, PartialEq)]
pub enum RowPlan {
    /// A whole model text against a parent (`child_v2` as a mirror).
    Mirror { parent: String, source: &'static str },
    /// A mixed row's one span against its parent's span.
    Span { parent: String, parent_span: (usize, usize), row_span: (usize, usize), source: &'static str },
    /// A parentless whole model text laid out like its donor.
    Donor { donor: String },
    /// Dropped with a receipt.
    Drop { reason: &'static str },
}

/// One bundle row, as the rows re-verdict reads it.
#[derive(Clone, Debug)]
pub struct Row {
    pub id: String,
    pub text: String,
    pub origin: String,
    pub source_kind: String,
    pub cohort_id: String,
    pub source_group: String,
    pub domain: String,
    pub parent_id: Option<String>,
    pub ai_byte_spans: Vec<(usize, usize)>,
    pub weight: Option<f64>,
}

impl Row {
    pub fn from_value(v: &Value) -> Result<Row> {
        let id = v["id"].as_str().context("row id")?.to_string();
        let s = |k: &str| v[k].as_str().map(String::from);
        ensure!(v["split"] == "train", "row {id} is not a Train row (split {})", v["split"]);
        let text = s("text").with_context(|| format!("row {id}: text"))?;
        ensure!(v["text_sha256"] == sha(text.as_bytes()).as_str(), "row {id}: text digest");
        let mut spans = Vec::new();
        for x in v["ai_byte_spans"].as_array().into_iter().flatten() {
            let a = x.as_array().context("span")?;
            ensure!(a.len() == 2, "row {id}: span");
            spans.push((a[0].as_u64().context("span")? as usize, a[1].as_u64().context("span")? as usize));
        }
        Ok(Row {
            text,
            origin: s("origin").with_context(|| format!("row {id}: origin"))?,
            source_kind: s("source_kind").with_context(|| format!("row {id}: source_kind"))?,
            cohort_id: s("cohort_id").unwrap_or_default(),
            source_group: s("source_group").unwrap_or_default(),
            domain: s("domain").with_context(|| format!("row {id}: domain"))?,
            parent_id: s("parent_id"),
            ai_byte_spans: spans,
            weight: v["weight"].as_f64(),
            id,
        })
    }

    /// Whether the row's model text is all of it: model-only, or one span
    /// over the whole text.
    pub fn whole(&self) -> bool {
        self.origin == "model_only" || self.ai_byte_spans == [(0, self.text.len())]
    }
}

/// A RAID human document of the RAID pool the assembler read: its id,
/// source group, domain, text and RAID attack name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RaidHuman {
    pub id: String,
    pub source_group: String,
    pub domain: String,
    pub text: String,
    pub attack: String,
}

/// The RAID human rule (amendment 20).
pub const RAID_HUMAN_RULE: &str = "a RAID model row's RAID human is the one human_only document of its source group whose RAID attack is none, from the RAID pool documents the assembler read (the bundle's row when the bundle holds it, else the pool's document); attacked human variants are never layout sources or parents; a group with none or several such documents gives no RAID human, and its rows are dropped with a receipt";
/// The stage-3 and capture parent rule (amendment 20).
pub const STAGE3_PARENT_RULE: &str = "a stage-3 legacy row or hosted capture with a recorded parent takes it when it is a human_only row of the rows (a mixed row's parent span from its unchanged prefix and suffix); a stage-3 legacy row with no recorded parent and one span that leaves text on either side takes the one stage-3 legacy human_only row of its source group whose bytes equal the row's outside the span; a row with no recorded parent whose model text is all of it takes a layout donor; any other row is dropped with a receipt";
/// The layout donor rule (amendment 20).
pub const DONOR_RULE: &str = "layout donor: the same-domain Train-2 human with the smallest V21 plan key, x = layout_donor, d.id = row id + ':' + human id (ties to the smaller human id); at the section 4.3 re-verdict the pool is every human_only row with weight > 0 of R0's Train build rows (the rows file itself unless --donors names another); donors transfer layout properties only and are no parents: they do not enter the prior, and a donor row's conventions are keyed draws from the prior of the batch's parents";

/// The RAID humans that `docs` (the RAID pool documents, JSONL) holds for
/// the source groups `groups`: every `human_only` document of those groups
/// with its attack name, its text digest checked. Lines without the bytes
/// `"human_only"` are skipped before parsing.
pub fn raid_humans(docs: &Path, groups: &BTreeSet<&str>) -> Result<Vec<RaidHuman>> {
    let mut out = Vec::new();
    let f = BufReader::new(fs::File::open(docs).with_context(|| docs.display().to_string())?);
    for (n, l) in f.split(b'\n').enumerate() {
        let l = l?;
        if !l.windows(12).any(|w| w == b"\"human_only\"") {
            continue;
        }
        let v: Value = serde_json::from_slice(&l).with_context(|| format!("{} line {}", docs.display(), n + 1))?;
        let g = v["source_group"].as_str().unwrap_or_default();
        if v["origin"] != "human_only" || !groups.contains(g) {
            continue;
        }
        let id = v["id"].as_str().context("RAID document id")?;
        ensure!(v["intended_use"] == "train_only" && v["split_assigned"] == false, "RAID document {id} is not an unassigned Train-only document");
        let text = v["text"].as_str().with_context(|| format!("RAID document {id}: text"))?;
        ensure!(v["text_sha256"] == sha(text.as_bytes()).as_str(), "RAID document {id}: text digest");
        let attack = v["raid"]["attack"]["name"].as_str().with_context(|| format!("RAID document {id}: attack name"))?;
        out.push(RaidHuman {
            id: id.into(),
            source_group: g.into(),
            domain: v["domain"].as_str().context("RAID document domain")?.into(),
            text: text.into(),
            attack: attack.into(),
        });
    }
    out.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(out)
}

/// Recovers a mixed row's parent span: the row is the parent with one span
/// replaced by `row[s..e]`, so the parent's prefix and suffix are the row's.
pub fn parent_span_of(parent: &str, row: &str, (s, e): (usize, usize)) -> Option<(usize, usize)> {
    let tail = row.len().checked_sub(e)?;
    let pe = parent.len().checked_sub(tail)?;
    (s <= pe && parent.get(..s)? == row.get(..s)? && parent.get(pe..)? == row.get(e..)?).then_some((s, pe))
}

/// A mixed row's plan against parent `pr` (`source`): its one span's
/// parent span, recovered from the unchanged prefix and suffix.
fn span_plan(r: &Row, pr: &Row, source: &'static str) -> RowPlan {
    if r.whole() {
        return RowPlan::Mirror { parent: pr.id.clone(), source };
    }
    let [sp] = r.ai_byte_spans.as_slice() else { return RowPlan::Drop { reason: "renorm_mixed_row_not_one_span" } };
    match parent_span_of(&pr.text, &r.text, *sp) {
        Some(ps) if ps.0 < ps.1 => RowPlan::Span { parent: pr.id.clone(), parent_span: ps, row_span: *sp, source },
        Some(_) => RowPlan::Drop { reason: "renorm_insertion_unsupported" },
        None => RowPlan::Drop { reason: "renorm_parent_span_unrecoverable" },
    }
}

/// The rows re-verdict's plan for every non-human row of `rows` (see the
/// module notes; `RAID_HUMAN_RULE`, `STAGE3_PARENT_RULE`, `DONOR_RULE`):
/// RAID rows against their RAID human of `raid` (whose ids are bundle row
/// ids when the bundle holds them), stage-3 legacy rows and hosted captures
/// against a recorded or recovered parent or a layout donor of `donors`.
/// Human rows and rows of other source kinds get no plan.
pub fn plan_rows(rows: &[Row], raid: &[RaidHuman], donors: &[Donor]) -> Vec<Option<RowPlan>> {
    let mut stage3_humans: HashMap<&str, Vec<usize>> = HashMap::new();
    let mut by_id: HashMap<&str, usize> = HashMap::new();
    for (i, r) in rows.iter().enumerate() {
        by_id.insert(r.id.as_str(), i);
        if r.origin == "human_only" && r.source_kind == "stage3_legacy" {
            stage3_humans.entry(r.source_group.as_str()).or_default().push(i);
        }
    }
    let mut unattacked: HashMap<&str, Vec<&RaidHuman>> = HashMap::new();
    for h in raid.iter().filter(|h| h.attack == "none") {
        unattacked.entry(h.source_group.as_str()).or_default().push(h);
    }
    let mut by_domain: HashMap<&str, Vec<Donor>> = HashMap::new();
    for d in donors {
        by_domain.entry(d.domain.as_str()).or_default().push(d.clone());
    }
    let donor = |r: &Row| match pick_donor(&r.id, &r.domain, by_domain.get(r.domain.as_str()).map(Vec::as_slice).unwrap_or_default()) {
        Some(d) => RowPlan::Donor { donor: d.id.clone() },
        None => RowPlan::Drop { reason: "renorm_donor_missing" },
    };
    rows.iter()
        .map(|r| {
            if r.origin == "human_only" {
                return None;
            }
            match r.source_kind.as_str() {
                "external_pool" if r.cohort_id.starts_with(RAID_COHORT_PREFIX) => Some(if r.origin != "model_only" {
                    RowPlan::Drop { reason: "renorm_raid_row_not_model_only" }
                } else {
                    match unattacked.get(r.source_group.as_str()).map(Vec::as_slice).unwrap_or_default() {
                        [h] => RowPlan::Mirror { parent: h.id.clone(), source: "raid_human" },
                        [] => RowPlan::Drop { reason: "renorm_raid_human_missing" },
                        _ => RowPlan::Drop { reason: "renorm_raid_human_ambiguous" },
                    }
                }),
                "stage3_legacy" | "hosted_capture" => Some(match &r.parent_id {
                    Some(p) => match by_id.get(p.as_str()).map(|&i| &rows[i]) {
                        Some(pr) if pr.origin == "human_only" => span_plan(r, pr, "recorded_parent"),
                        Some(_) => RowPlan::Drop { reason: "renorm_recorded_parent_not_human" },
                        None => RowPlan::Drop { reason: "renorm_recorded_parent_missing" },
                    },
                    None if r.whole() => donor(r),
                    None if r.source_kind == "stage3_legacy" && r.ai_byte_spans.len() == 1 => {
                        let hs = stage3_humans.get(r.source_group.as_str()).map(Vec::as_slice).unwrap_or_default();
                        let fit: Vec<&Row> = hs.iter().map(|&i| &rows[i]).filter(|h| parent_span_of(&h.text, &r.text, r.ai_byte_spans[0]).is_some()).collect();
                        match fit.as_slice() {
                            [pr] => span_plan(r, pr, "stage3_source_group"),
                            [] => RowPlan::Drop { reason: "renorm_stage3_parent_unrecoverable" },
                            _ => RowPlan::Drop { reason: "renorm_stage3_parent_ambiguous" },
                        }
                    }
                    None if r.ai_byte_spans.len() == 1 => RowPlan::Drop { reason: "renorm_mixed_row_without_parent" },
                    None => RowPlan::Drop { reason: "renorm_mixed_row_not_one_span" },
                }),
                _ => None,
            }
        })
        .collect()
}

/// The window on a final document, as the generators record it.
fn window(doc: &str, kind: &str, reasons: &mut Vec<String>, record: &mut Value) {
    let (lo, hi) = LENGTH_WINDOW;
    let n = grammar_a_form::text::words(doc);
    record["length_window"] = json!({"id": LENGTH_WINDOW_ID, "min_words": lo, "max_words": hi, "document": kind, "words": n});
    reasons.extend(grammar_a_form::window::reasons(n, LENGTH_WINDOW));
    reasons.sort();
    reasons.dedup();
}

/// The Train rows of a bundle rows file: lines whose `split` (read from the
/// line's bytes) is `train` are parsed; others are skipped unparsed and
/// counted.
fn train_rows(p: &Path, input: &str, skipped: &mut Skipped) -> Result<Vec<Row>> {
    read_split_lines(p, "split", &|s| s == "train", input, skipped)?.iter().map(Row::from_value).collect()
}

/// Re-verdicts and normalises Train rows (see the module notes): every RAID,
/// stage-3 legacy and hosted-capture row of `rows_path` (bundle rows; only
/// lines whose split is `train` are parsed), with RAID humans from
/// `raid_docs` (the RAID pool documents the assembler read; needed when the
/// rows hold RAID rows) and layout donors from `donors_path` (default: the
/// rows file; its `human_only` rows with weight > 0). Writes, in the new
/// directory `out`: `rows-reverdict.jsonl` (every planned row; no text),
/// `rows-normalised.jsonl` (the rows that pass; text), the prior record
/// and `ROWS-RENORM.json`. Returns the summary. With `plan_only`, it stops
/// after the plan and writes only `ROWS-PLAN.json`: the planned rows by
/// class and layout source or drop reason (counts, no text, no ids, no
/// layout or verdict).
pub fn renorm_rows(rows_path: &Path, donors_path: Option<&Path>, raid_docs: Option<&Path>, population: &str, out: &Path, plan_only: bool) -> Result<Value> {
    ensure!(is_r2_population(population), "population {population} is not a round-2 population id (protocol A2 amendment 10 item 10.8)");
    let mut skipped = Skipped::new();
    let rows = train_rows(rows_path, "rows", &mut skipped)?;
    let mut seen = HashSet::new();
    for r in &rows {
        ensure!(seen.insert(r.id.as_str()), "duplicate row id {}", r.id);
    }
    let donors_path = donors_path.unwrap_or(rows_path);
    let mut donors: Vec<Donor> = Vec::new();
    let mut not_positive = 0usize;
    let mut dseen = HashSet::new();
    let other: Vec<Row>;
    let pool: &[Row] = if donors_path == rows_path {
        &rows
    } else {
        other = train_rows(donors_path, "donors", &mut skipped)?;
        &other
    };
    for d in pool {
        if d.origin != "human_only" {
            ensure!(donors_path == rows_path, "donor {} is not a human_only row", d.id);
            continue;
        }
        ensure!(dseen.insert(d.id.as_str()), "duplicate donor id {}", d.id);
        if !d.weight.is_some_and(|w| w > 0.0) {
            not_positive += 1;
            continue;
        }
        donors.push(Donor { id: d.id.clone(), domain: d.domain.clone(), text: d.text.clone() });
    }
    donors.sort_by(|a, b| a.id.cmp(&b.id));
    // RAID humans: the pool documents of the RAID model rows' groups
    let raid_groups: BTreeSet<&str> = rows
        .iter()
        .filter(|r| r.source_kind == "external_pool" && r.cohort_id.starts_with(RAID_COHORT_PREFIX) && r.origin == "model_only")
        .map(|r| r.source_group.as_str())
        .collect();
    let by_id: HashMap<&str, &Row> = rows.iter().map(|r| (r.id.as_str(), r)).collect();
    let mut raid: Vec<RaidHuman> = Vec::new();
    let mut raid_input = Value::Null;
    if !raid_groups.is_empty() {
        let p = raid_docs.context("the rows hold RAID rows: --raid-documents names the RAID pool documents the assembler read")?;
        raid = raid_humans(p, &raid_groups)?;
        let mut in_rows = 0usize;
        for h in &raid {
            if let Some(r) = by_id.get(h.id.as_str()) {
                ensure!(r.origin == "human_only" && r.text == h.text && r.source_group == h.source_group, "RAID document {} differs from its bundle row", h.id);
                in_rows += 1;
            }
        }
        let bundle_raid_humans = rows.iter().filter(|r| r.origin == "human_only" && r.cohort_id.starts_with(RAID_COHORT_PREFIX) && raid_groups.contains(r.source_group.as_str())).count();
        ensure!(in_rows == bundle_raid_humans, "{bundle_raid_humans} RAID human rows of the rows' RAID groups, {in_rows} found among the RAID documents");
        let mut attacks: BTreeMap<&str, usize> = BTreeMap::new();
        for h in &raid {
            *attacks.entry(h.attack.as_str()).or_default() += 1;
        }
        raid_input = json!({"path": p, "sha256": file_sha(p)?, "human_documents_of_the_groups": raid.len(), "of_them_bundle_rows": in_rows, "by_attack": attacks});
    }
    let raid_by_id: HashMap<&str, &RaidHuman> = raid.iter().map(|h| (h.id.as_str(), h)).collect();
    let plans = plan_rows(&rows, &raid, &donors);
    if plan_only {
        let mut by: BTreeMap<String, BTreeMap<String, usize>> = BTreeMap::new();
        for (r, p) in rows.iter().zip(&plans) {
            let Some(p) = p else { continue };
            let class = if r.cohort_id.starts_with(RAID_COHORT_PREFIX) { "raid".to_string() } else { format!("{}:{}", r.source_kind, if r.whole() { "whole" } else { "span" }) };
            let k = match p {
                RowPlan::Mirror { parent, source } => format!("{source}{}", if by_id.contains_key(parent.as_str()) { "" } else { ":pool_document" }),
                RowPlan::Span { source, .. } => source.to_string(),
                RowPlan::Donor { .. } => "layout_donor".into(),
                RowPlan::Drop { reason } => reason.to_string(),
            };
            *by.entry(class).or_default().entry(k).or_default() += 1;
        }
        new_dir(out)?;
        let summary = json!({"schema": "slop_ninja_a_rows_renorm_plan_v1", "code": crate::CODE_ID, "population": population, "model_calls": NO_MODEL_CALL,
            "inputs": {"rows": {"path": rows_path, "sha256": file_sha(rows_path)?, "rows": rows.len()}, "donors": {"donors": donors.len(), "weight_not_positive_excluded": not_positive},
                "raid_documents": raid_input},
            "planned_by_class": by, "planned": plans.iter().flatten().count(), "lines_skipped_unparsed": skipped,
            "rules": {"raid_human": RAID_HUMAN_RULE, "stage3_parent": STAGE3_PARENT_RULE, "donor": DONOR_RULE, "splits": SPLIT_RULE},
            "note": "the plan only: no layout, screen, echo or window ran; counts, no text and no ids"});
        write_new(&out.join("ROWS-PLAN.json"), &pretty(&summary)?)?;
        return Ok(summary);
    }
    let donor_by_id: HashMap<&str, &Donor> = donors.iter().map(|d| (d.id.as_str(), d)).collect();
    // a parent's (domain, text): a row, else a RAID pool document
    let parent_of = |id: &str| -> (&str, &str, bool) {
        match by_id.get(id) {
            Some(r) => (r.domain.as_str(), r.text.as_str(), true),
            None => {
                let h = raid_by_id[id];
                (h.domain.as_str(), h.text.as_str(), false)
            }
        }
    };
    // the batch's prior holds its parents only (item 7.8): RAID humans and recorded or recovered parents
    let mut parents: BTreeMap<&str, (&str, &str)> = BTreeMap::new();
    for p in plans.iter().flatten() {
        if let RowPlan::Mirror { parent, .. } | RowPlan::Span { parent, .. } = p {
            let (d, t, _) = parent_of(parent);
            parents.insert(parent.as_str(), (d, t));
        }
    }
    let prior = PriorV2::of(population, parents.values().copied());
    let seed = norm_prior_seed(population);
    let listed: String = parents.iter().map(|(id, (d, t))| format!("{id}\t{d}\t{}\n", sha(t.as_bytes()))).collect();
    let prior_rec = json!({"schema": ROWS_PRIOR_SCHEMA, "profile": PROFILE_V2, "normaliser": NORM_ID, "population": population, "seed": seed,
        "key_scope": grammar_a_form::conventions::KEY_SCOPE, "parents": parents.len(), "parents_sha256": sha(listed.as_bytes()), "prior": prior,
        "parents_by_domain": prior.by_domain.iter().map(|(d, p)| (d.clone(), p.parents)).collect::<BTreeMap<_, _>>(),
        "rule": "the rows batch's parents (RAID humans and recorded or recovered parents; layout donors are not parents), each in its own domain; a row's keyed draw (KEY_SCOPE, this seed, the row id) uses its domain cell, else the population-wide prior, else none"});
    let prior_bytes = serde_json::to_vec(&prior_rec)?;
    let prior_sha = sha(&prior_bytes);
    let mut reverdicts = Vec::new();
    let mut normalised = Vec::new();
    let mut by_class: BTreeMap<String, BTreeMap<&str, usize>> = BTreeMap::new();
    let mut drop_reasons: BTreeMap<String, usize> = BTreeMap::new();
    let mut plan_sources: BTreeMap<String, usize> = BTreeMap::new();
    let mut skipped_kinds: BTreeMap<String, usize> = BTreeMap::new();
    for (r, p) in rows.iter().zip(&plans) {
        let Some(p) = p else {
            if r.origin != "human_only" {
                *skipped_kinds.entry(r.source_kind.clone()).or_default() += 1;
            }
            continue;
        };
        let class = if r.cohort_id.starts_with(RAID_COHORT_PREFIX) { "raid".to_string() } else { r.source_kind.clone() };
        let (mut reasons, mut record, doc, spans, source) = match p {
            RowPlan::Drop { reason } => (vec![reason.to_string()], json!({"profile": PROFILE_V2}), None, Vec::new(), Value::Null),
            RowPlan::Mirror { parent, source } => {
                let (domain, ptext, in_rows) = parent_of(parent);
                let c = counts(ptext);
                let conv = resolve_v2(&c, &prior, domain, &seed, &r.id);
                let ch = fprofile::child_v2(ptext, None, &conv, &r.text);
                let mut rec = ch.record;
                rec["parent_counts"] = json!(c);
                let n = ch.text.len();
                (ch.reasons, rec, (!ch.text.is_empty()).then_some(ch.text), vec![(0, n)], json!({"kind": source, "id": parent, "domain": domain, "bundle_row": in_rows}))
            }
            RowPlan::Span { parent, parent_span, row_span, source } => {
                let (domain, ptext, _) = parent_of(parent);
                let c = counts(ptext);
                let conv = resolve_v2(&c, &prior, domain, &seed, &r.id);
                let raw = &r.text[row_span.0..row_span.1];
                let ch = fprofile::child_v2(ptext, Some(*parent_span), &conv, raw);
                let mut rec = ch.record;
                rec["parent_counts"] = json!(c);
                let mut reasons = ch.reasons;
                let doc = if ch.text.is_empty() {
                    None
                } else {
                    match fprofile::splice(ptext, *parent_span, &ch.text) {
                        Ok(d) => Some(d),
                        Err(e) => {
                            reasons.push(format!("window_assembly_error:{e}"));
                            None
                        }
                    }
                };
                // the final span starts after a leading line break and lead
                // (amendment 39 item 39.2, as an item's edit's does)
                let lead = fprofile::replacement_lead(&ch.text).unwrap_or(0);
                let sp = vec![(parent_span.0 + lead, parent_span.0 + ch.text.len())];
                (reasons, rec, doc, sp, json!({"kind": source, "id": parent, "domain": domain, "parent_span": [parent_span.0, parent_span.1]}))
            }
            RowPlan::Donor { donor } => {
                let d = donor_by_id[donor.as_str()];
                let conv = resolve_v2(&Counts::default(), &prior, &r.domain, &seed, &r.id);
                let ch = fprofile::donor_child_v2(&SourceLayout::whole(&d.text), &conv, &r.text);
                let n = ch.text.len();
                (ch.reasons, ch.record, (!ch.text.is_empty()).then_some(ch.text), vec![(0, n)],
                    json!({"kind": "layout_donor", "id": d.id, "domain": d.domain, "key": donor_key(&r.id, &d.id), "rule": DONOR_RULE}))
            }
        };
        if !source.is_null() {
            *plan_sources.entry(format!("{class}:{}", source["kind"].as_str().unwrap_or_default())).or_default() += 1;
        }
        if let Some(d) = &doc {
            let kind = if matches!(p, RowPlan::Span { .. }) { "the parent with the laid-out span" } else { "the laid-out row" };
            window(d, kind, &mut reasons, &mut record);
        }
        reasons.sort();
        reasons.dedup();
        let pass = reasons.is_empty() && doc.is_some();
        let action = if pass { "keep" } else { "drop_with_receipt" };
        *by_class.entry(class.clone()).or_default().entry(action).or_default() += 1;
        if !pass {
            for x in &reasons {
                *drop_reasons.entry(x.clone()).or_default() += 1;
            }
        }
        record["population"] = json!(population);
        record["prior_sha256"] = json!(prior_sha);
        record["key"] = json!({"rule": "the row id", "value": r.id});
        let doc_sha = doc.as_ref().map(|d| sha(d.as_bytes()));
        reverdicts.push(json!({"schema": ROWS_REVERDICT_SCHEMA, "id": r.id, "class": class, "source_kind": r.source_kind, "cohort_id": r.cohort_id,
            "origin": r.origin, "domain": r.domain, "layout_source": source, "pass": pass, "reasons": reasons, "action": action,
            "raw_text_sha256": sha(r.text.as_bytes()), "text_sha256": doc_sha, "record": record}));
        if let (true, Some(d)) = (pass, doc) {
            let share = match p {
                RowPlan::Span { .. } => {
                    let (a, b) = spans[0];
                    text::words(&d[a..b]) as f64 / text::words(&d).max(1) as f64
                }
                _ => 1.0,
            };
            normalised.push(json!({"schema": ROWS_NORMALISED_SCHEMA, "id": r.id, "profile": PROFILE_V2, "population": population, "prior_sha256": prior_sha,
                "text": d, "text_sha256": sha(d.as_bytes()), "origin": r.origin, "ai_byte_spans": spans.iter().map(|&(a, b)| [a, b]).collect::<Vec<_>>(), "share": share,
                "raw_text_sha256": sha(r.text.as_bytes()), "raw_ai_byte_spans": r.ai_byte_spans.iter().map(|&(a, b)| [a, b]).collect::<Vec<_>>(),
                "layout_source": source}));
        }
    }
    new_dir(out)?;
    let prior_name = format!("form-prior-{prior_sha}.json");
    write_new(&out.join(&prior_name), &prior_bytes)?;
    let rv_sha = write_new(&out.join("rows-reverdict.jsonl"), &jsonl_bytes(&reverdicts)?)?;
    let nm_sha = write_new(&out.join("rows-normalised.jsonl"), &jsonl_bytes(&normalised)?)?;
    let summary = json!({"schema": ROWS_RENORM_SCHEMA, "code": crate::CODE_ID, "profile": fprofile::Profile::CohortV2.record(), "population": population,
        "seed": seed, "prior_sha256": prior_sha, "model_calls": NO_MODEL_CALL,
        "inputs": {"rows": {"path": rows_path, "sha256": file_sha(rows_path)?, "rows": rows.len()},
            "donors": {"path": donors_path, "same_as_rows": donors_path == rows_path, "sha256": file_sha(donors_path)?, "donors": donors.len(), "weight_not_positive_excluded": not_positive},
            "raid_documents": raid_input},
        "counts": {"planned": reverdicts.len(), "kept": normalised.len(), "by_class_and_action": by_class, "drop_reasons": drop_reasons,
            "layout_sources_by_class": plan_sources, "non_human_rows_without_a_plan_by_source_kind": skipped_kinds, "parents": parents.len(),
            "lines_skipped_unparsed": skipped,
            "document_line_splits": reverdicts.iter().map(|r| grammar_a_form::profile::document_line_splits(&r["record"])).sum::<u64>()},
        "outputs": {"prior": prior_name, "reverdict": {"path": "rows-reverdict.jsonl", "sha256": rv_sha}, "normalised": {"path": "rows-normalised.jsonl", "sha256": nm_sha}},
        "consumer": ROWS_CONSUMER,
        "rules": {"raid_human": RAID_HUMAN_RULE, "stage3_parent": STAGE3_PARENT_RULE, "donor": DONOR_RULE, "splits": SPLIT_RULE,
            "prior": "the rows batch's parents only, each in its own domain, under the population's seed; draws are keyed by the row id",
            "window": "cc_final_length_window_v1 on every laid-out document: the laid-out row, or the parent with the laid-out span"}});
    write_new(&out.join("ROWS-RENORM.json"), &pretty(&summary)?)?;
    Ok(summary)
}

/// What consumes `rows-normalised.jsonl`.
pub const ROWS_CONSUMER: &str = "rows-normalised.jsonl replaces, by bundle row id, the text, text_sha256, ai_byte_spans and share of the RAID, stage-3 legacy and hosted-capture Train rows it lists (origin and every other field unchanged); a planned row it does not list is dropped with its receipt in rows-reverdict.jsonl; raw_text_sha256 and raw_ai_byte_spans name the raw twin (the bundle row as it was); no landed assembler input reads it yet (open item)";

#[cfg(test)]
mod tests {
    use super::*;
    use crate::generation::Ctx;
    use crate::overlap::{Overlap, PriorIndex};
    use std::sync::Arc;

    /// Synthetic vocabularies (no corpus text).
    const SYN: [&str; 20] = [
        "amber", "basalt", "cobalt", "delta", "ember", "fjord", "garnet", "harbor", "indigo", "juniper", "kestrel", "lantern", "meadow",
        "nectar", "orchid", "pebble", "quartz", "river", "saffron", "timber",
    ];
    const ALT: [&str; 20] = [
        "acorn", "bramble", "canyon", "dune", "estuary", "fern", "glacier", "heather", "inlet", "jasper", "knoll", "lichen", "marsh",
        "nettle", "oasis", "prairie", "quarry", "ridge", "sorrel", "thicket",
    ];

    fn prose(vocab: &[&str], seed: u64, n: usize, len: usize) -> String {
        let mut st = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        let mut out: Vec<String> = Vec::new();
        for _ in 0..n {
            let ws: Vec<&str> = (0..len)
                .map(|_| {
                    st = st.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                    vocab[((st >> 33) as usize) % vocab.len()]
                })
                .collect();
            let s = ws.join(" ");
            out.push(format!("{}{}.", s[..1].to_uppercase(), &s[1..]));
        }
        out.join(" ")
    }

    /// P1: three paragraphs, about 200 words, curly double quotes.
    fn p1() -> String {
        let a = format!("{} The keeper called it \u{201C}amber harbor\u{201D} again. {}", prose(&SYN, 1, 3, 11), prose(&SYN, 2, 2, 10));
        let b = prose(&SYN, 3, 6, 11);
        let c = format!("{} They named the shore \u{201C}quiet fjord\u{201D} later. {}", prose(&SYN, 4, 2, 12), prose(&SYN, 5, 3, 11));
        format!("{a}\n\n{b}\n\n{c}")
    }

    /// P2: one paragraph of about 160 words.
    fn p2() -> String {
        prose(&SYN, 7, 14, 12)
    }

    fn span_of(p: &str, i: usize) -> (usize, usize) {
        let s = text::sentences(p)[i];
        (s.start, s.end)
    }

    fn tokenizer() -> Tokenizer {
        r#"{"version":"1.0","truncation":null,"padding":null,"added_tokens":[],"normalizer":null,"pre_tokenizer":{"type":"Whitespace"},"post_processor":null,"decoder":null,"model":{"type":"WordLevel","vocab":{"[UNK]":0},"unk_token":"[UNK]"}}"#
            .parse()
            .unwrap()
    }

    fn request(id: &str, item: &str, parent: &str, span: Option<(usize, usize)>, target: usize) -> Request {
        Request {
            request_id: id.into(),
            item_id: item.into(),
            parent_id: parent.into(),
            split: "train".into(),
            generator: "g".into(),
            endpoint: "chat".into(),
            template_id: "t".into(),
            prompt: format!("prompt {id}"),
            span_index: 0,
            source_span: span,
            target_words: target,
            cut_budget: None,
            decoding: plan::profile("A", 0.7, 0.9, 1536),
            request_sha256: sha(id.as_bytes()),
            seed: 7,
            op: None,
            cohort: None,
            attempt_limit: None,
        }
    }

    fn parents() -> HashMap<String, String> {
        [("p1".to_string(), p1()), ("p2".to_string(), p2())].into()
    }

    fn form(pop: &str) -> FormPost {
        let ps = parents();
        FormPost::new(pop, [("p1", "fiction", ps["p1"].as_str()), ("p2", "news", ps["p2"].as_str())]).unwrap()
    }

    /// A test context (tests only: the renorm path never builds one).
    fn ctx(post: Option<Arc<dyn PostProcess>>) -> Ctx {
        Ctx {
            endpoint: String::new(),
            alias: "m".into(),
            config: json!({}),
            tokenizer: tokenizer(),
            overlap: Overlap::new(PriorIndex::empty()),
            parents: parents(),
            client: generation::client().unwrap(),
            post,
            doc_words: None,
        }
    }

    fn chat_body(content: &str) -> Value {
        json!({"choices":[{"message":{"content":content},"finish_reason":"stop"}],"usage":{},"timings":{}})
    }

    /// The journal lines of one request's attempts: each (attempt, reply)
    /// gets an intent and a response whose verdict is the profile-off
    /// verdict of `verdict_of` (`generation::verdict` with no post step, or
    /// a spliced verdict).
    fn lines_of(r: &Request, attempts: &[(u32, &str)], verdict_of: &dyn Fn(&Request, &Value) -> Value) -> Vec<Value> {
        let mut v = Vec::new();
        for (a, reply) in attempts {
            let parsed = chat_body(reply);
            v.push(json!({"kind":"intent","request_id":r.request_id,"item_id":r.item_id,"split":r.split,"generator":r.generator,"attempt":a,"seed":r.seed + a,"t":1,"body":{"prompt":r.prompt}}));
            v.push(json!({"kind":"response","request_id":r.request_id,"item_id":r.item_id,"split":r.split,"generator":r.generator,"attempt":a,"seed":r.seed + a,
                "t_start":1,"t_end":2,"elapsed_ms":1,"status":200,"raw_body":serde_json::to_string(&parsed).unwrap(),"timings":{},"usage":{},"verdict":verdict_of(r, &parsed)}));
        }
        v
    }

    fn write_journal(runs: &Path, generator: &str, name: &str, lines: &[Value]) {
        let d = runs.join(generator);
        fs::create_dir_all(&d).unwrap();
        fs::write(d.join(name), jsonl_bytes(lines).unwrap()).unwrap();
    }

    fn ritem(id: &str, parent: &str, kind: &str, reqs: &[&Request]) -> RItem {
        let spec = (reqs.len() > 1).then(|| ItemSpec { item_id: id.into(), parts: reqs.iter().map(|r| (r.request_id.clone(), r.source_span.unwrap(), None)).collect() });
        RItem {
            item_id: id.into(),
            parent_id: parent.into(),
            kind: kind.into(),
            cohort: "primary".into(),
            domain: if parent == "p1" { "fiction".into() } else { "news".into() },
            requests: reqs.iter().map(|r| r.request_id.clone()).collect(),
            spec,
        }
    }

    /// The local fixture: requests, items and their journals.
    struct Fixture {
        dir: tempfile::TempDir,
        reqs: Vec<Request>,
        items: Vec<RItem>,
    }

    fn fixture() -> Fixture {
        let ps = parents();
        let (q1, q2) = (&ps["p1"], &ps["p2"]);
        let w1 = text::words(q1);
        let off = ctx(None);
        let vf = |r: &Request, parsed: &Value| generation::verdict(&off, r, 200, parsed);
        let s_list = span_of(q1, 9);
        let s_a = span_of(q1, 1);
        let s_b = span_of(q1, 12);
        let sw = |sp: (usize, usize)| text::words(&q1[sp.0..sp.1]);
        let m_ok = request("r-m-ok", "i-m-ok", "p1", None, w1);
        let m_echo = request("r-m-echo", "i-m-echo", "p1", None, w1);
        let m_late = request("r-m-late", "i-m-late", "p1", None, w1);
        let s_lst = request("r-s-list", "i-s-list", "p1", Some(s_list), sw(s_list));
        let ma = request("r-x-a", "i-x", "p1", Some(s_a), sw(s_a));
        let mb = request("r-x-b", "i-x", "p1", Some(s_b), sw(s_b));
        let ea = request("r-y-a", "i-y", "p1", Some(s_a), sw(s_a));
        let eb = request("r-y-b", "i-y", "p1", Some(s_b), sw(s_b));
        let inc = request("r-inc", "i-inc", "p2", None, text::words(q2));
        let other = {
            let mut r = request("r-dev", "i-dev", "p2", None, text::words(q2));
            r.split = "dev_iter".into();
            r
        };
        let fresh = prose(&ALT, 11, 18, 11);
        let list = "- Acorn bramble canyon dune estuary fern.\n- Glacier heather inlet jasper knoll lichen.".to_string();
        let ra = prose(&ALT, 31, 1, sw(s_a));
        let rb = prose(&ALT, 32, 1, sw(s_b));
        let dir = tempfile::tempdir().unwrap();
        let runs = dir.path().join("runs");
        let mut g: Vec<Value> = Vec::new();
        g.extend(lines_of(&m_ok, &[(0, &fresh)], &vf));
        g.extend(lines_of(&m_echo, &[(0, q1)], &vf));
        g.extend(lines_of(&m_late, &[(0, ""), (1, q1)], &vf));
        g.extend(lines_of(&s_lst, &[(0, &list)], &vf));
        g.extend(lines_of(&ma, &[(0, &ra)], &vf));
        g.extend(lines_of(&mb, &[(0, &rb)], &vf));
        g.extend(lines_of(&ea, &[(0, &q1[s_a.0..s_a.1].replacen(' ', "  ", 1))], &vf));
        g.extend(lines_of(&eb, &[(0, &rb)], &vf));
        g.extend(lines_of(&inc, &[(0, "")], &vf));
        g.extend(lines_of(&other, &[(0, &fresh)], &vf));
        write_journal(&runs, "g", "journal-1-w0.jsonl", &g);
        // a torn last line is skipped
        let mut f = fs::OpenOptions::new().append(true).open(runs.join("g/journal-1-w0.jsonl")).unwrap();
        f.write_all(b"{\"kind\":\"resp").unwrap();
        let items = vec![
            ritem("i-m-ok", "p1", "mirror", &[&m_ok]),
            ritem("i-m-echo", "p1", "mirror", &[&m_echo]),
            ritem("i-m-late", "p1", "mirror", &[&m_late]),
            ritem("i-s-list", "p1", "span", &[&s_lst]),
            ritem("i-x", "p1", "span", &[&ma, &mb]),
            ritem("i-y", "p1", "span", &[&ea, &eb]),
            ritem("i-inc", "p2", "mirror", &[&inc]),
        ];
        let reqs = vec![m_ok, m_echo, m_late, s_lst, ma, mb, ea, eb, inc];
        Fixture { dir, reqs, items }
    }

    fn batch<'a>(fx: &Fixture, tok: &'a Tokenizer) -> Batch<'a> {
        Batch {
            kind: PlanKind::Local,
            name: "split-train".into(),
            form: form("train2"),
            requests: fx.reqs.iter().map(|r| (r.request_id.clone(), r.clone())).collect(),
            items: fx.items.clone(),
            parents: parents(),
            tokenizer: Some(tok),
            inputs: json!({"fixture": true}),
            split: "train".into(),
            skipped: Skipped::new(),
            records: None,
        }
    }

    fn lines(p: &Path) -> Vec<Value> {
        fs::read_to_string(p).unwrap().lines().map(|l| serde_json::from_str(l).unwrap()).collect()
    }

    /// Every file under `d`, relative path -> bytes.
    fn tree(d: &Path) -> BTreeMap<String, Vec<u8>> {
        let mut m = BTreeMap::new();
        let mut stack = vec![d.to_path_buf()];
        while let Some(x) = stack.pop() {
            for e in fs::read_dir(&x).unwrap() {
                let p = e.unwrap().path();
                if p.is_dir() {
                    stack.push(p);
                } else {
                    m.insert(p.strip_prefix(d).unwrap().display().to_string(), fs::read(&p).unwrap());
                }
            }
        }
        m
    }

    #[test]
    fn local_reverdict_is_the_generate_profile_verdict() {
        let ps = parents();
        let q1 = &ps["p1"];
        let w1 = text::words(q1);
        let tok = tokenizer();
        let fp = form("train2").with_items(Vec::new(), &HashMap::new()).unwrap();
        let on = ctx(Some(Arc::new(fp.clone())));
        let off = ctx(None);
        let s = span_of(q1, 9);
        let sw = text::words(&q1[s.0..s.1]);
        let mut cont = request("c1", "c1", "p1", Some(s), sw);
        cont.op = Some("middle_rewrite".into());
        let cases = [
            (request("m1", "m1", "p1", None, w1), prose(&ALT, 11, 18, 11)),
            (request("m2", "m2", "p1", None, w1), q1.clone()),
            (request("m3", "m3", "p1", None, w1), prose(&ALT, 12, 8, 11)),
            (request("s1", "s1", "p1", Some(s), sw), prose(&ALT, 13, 1, sw)),
            (request("s2", "s2", "p1", Some(s), sw), "- Acorn bramble canyon dune estuary fern.\n- Glacier heather inlet jasper knoll lichen.".into()),
            (request("s3", "s3", "p1", Some(s), sw), "The ranger said \"marsh nettle\" and walked on past the oasis ridge.".into()),
            (request("s4", "s4", "p1", Some(s), sw), q1[s.0..s.1].to_string()),
            (cont, prose(&ALT, 14, 1, sw)),
            // over the A-token limit (and the window)
            (request("m4", "m4", "p1", None, 2101), prose(&ALT, 15, 191, 11)),
        ];
        let mut rejected = 0;
        for (r, reply) in &cases {
            let parsed = chat_body(reply);
            let want = generation::verdict(&on, r, 200, &parsed);
            let orig = generation::verdict(&off, r, 200, &parsed);
            let raw = orig["text"].as_str().unwrap_or(reply);
            let got = reverdict_local(&fp, r, Some(q1), raw, &orig, &tok);
            assert_eq!(got, want, "{}", r.request_id);
            if got["accepted"] != true {
                rejected += 1;
            }
        }
        // the battery exercises both outcomes and the token limit
        assert!(rejected >= 3 && rejected < cases.len(), "{rejected}");
        let long = &cases[cases.len() - 1];
        let v = reverdict_local(&fp, &long.0, Some(q1), &long.1, &generation::verdict(&off, &long.0, 200, &chat_body(&long.1)), &tok);
        assert!(v["reasons"].to_string().contains("assembled_over_2048_a_tokens"), "{}", v["reasons"]);
    }

    fn sedit(id: &str, kind: &str, span: (usize, usize), sep: Option<&str>, source_words: usize) -> splice::Edit {
        let (lo, hi) = if kind == "insert" { (5.0, 40.0) } else { (source_words as f64 * 0.5, source_words as f64 * 1.5) };
        splice::Edit {
            edit_index: 0,
            component: kind.into(),
            kind: kind.into(),
            operation: kind.into(),
            family: "f".into(),
            operation_requested: kind.into(),
            generator: "g".into(),
            parent_span: span,
            insert_sep: sep.map(String::from),
            sentence_range: (0, 1),
            context_included: false,
            context_spans: Vec::new(),
            style: Value::Null,
            template_id: "t".into(),
            source_words,
            target_words: source_words.max(10),
            len_lo: lo,
            len_hi: hi,
            min_change: None,
            start_rule: "sentence_start".into(),
            end_rule: "terminal".into(),
            newline_ok: false,
            request_id: id.into(),
        }
    }

    #[test]
    fn spliced_reverdict_is_spliced_post() {
        let ps = parents();
        let q1 = &ps["p1"];
        let e_run = span_of(q1, 14);
        let e_fold = span_of(q1, 10);
        let para_end = span_of(q1, 5).1;
        let sw = |sp: (usize, usize)| text::words(&q1[sp.0..sp.1]);
        let cases = [
            (sedit("e1", "run", e_run, None, sw(e_run)), prose(&ALT, 21, 1, sw(e_run))),
            (sedit("e2", "insert", (para_end, para_end), Some(" "), 0), prose(&ALT, 22, 1, 12)),
            (sedit("e3", "run", e_fold, None, sw(e_fold)), q1[e_fold.0..e_fold.1].replacen(' ', "  ", 1)),
        ];
        let edits: HashMap<String, splice::Edit> = cases.iter().map(|(e, _)| (e.request_id.clone(), e.clone())).collect();
        let fp = form("span_select2").with_items(Vec::new(), &HashMap::new()).unwrap().with_edits(&edits);
        let on = ctx(Some(Arc::new(fp.clone())));
        let ov = Overlap::new(PriorIndex::empty());
        let mut outcomes = BTreeSet::new();
        for (e, reply) in &cases {
            let r = request(&e.request_id, &e.request_id, "p1", Some(e.parent_span), sw(e.parent_span).max(10));
            let orig = splice::verdict(e, q1, &ov, 200, &chat_body(reply));
            assert_eq!(orig["accepted"], true, "{}: {orig}", e.request_id);
            let mut want = orig.clone();
            form_post::spliced_post(&on, &r, q1, &mut want);
            let got = reverdict_spliced(&fp, &r, q1, orig["text"].as_str().unwrap(), &orig);
            assert_eq!(got, want, "{}", e.request_id);
            outcomes.insert(got["accepted"] == true);
        }
        assert_eq!(outcomes.len(), 2, "the battery exercises both outcomes");
    }

    /// A post step that is not profile v2's (B1c).
    struct NotV2;

    impl PostProcess for NotV2 {
        fn apply(&self, _r: &Request, _parent: Option<&str>, clean: &str) -> generation::PostOut {
            generation::PostOut { text: clean.replace(". ", ".  "), reasons: Vec::new(), record: json!({"step": "not_v2"}) }
        }
        fn is_profile_v2(&self) -> bool {
            false
        }
    }

    #[test]
    fn reverdicts_keep_raw_text_only_under_profile_v2() {
        // the pins above hold for a step that is not profile v2's too: the
        // re-verdicts keep no raw_text, as generation::verdict and
        // spliced_post keep none, and profile v2's step keeps it
        let ps = parents();
        let q1 = &ps["p1"];
        let w1 = text::words(q1);
        let tok = tokenizer();
        let on = ctx(Some(Arc::new(NotV2)));
        let off = ctx(None);
        let fp = form("train2").with_items(Vec::new(), &HashMap::new()).unwrap();
        let s = span_of(q1, 9);
        let sw = text::words(&q1[s.0..s.1]);
        for (r, reply) in [(request("m1", "m1", "p1", None, w1), prose(&ALT, 11, 18, 11)), (request("s1", "s1", "p1", Some(s), sw), prose(&ALT, 13, 1, sw))] {
            let parsed = chat_body(&reply);
            let orig = generation::verdict(&off, &r, 200, &parsed);
            let raw = orig["text"].as_str().unwrap();
            let got = reverdict_local(&NotV2, &r, Some(q1), raw, &orig, &tok);
            assert_eq!(got, generation::verdict(&on, &r, 200, &parsed), "{}", r.request_id);
            assert!(got.get("raw_text").is_none(), "{got}");
            assert_eq!(got["diag"]["post"]["step"], "not_v2");
            assert_eq!(reverdict_local(&fp, &r, Some(q1), raw, &orig, &tok)["raw_text"].as_str(), Some(raw));
        }
        let e_run = span_of(q1, 14);
        let ew = text::words(&q1[e_run.0..e_run.1]);
        let e = sedit("e1", "run", e_run, None, ew);
        let edits: HashMap<String, splice::Edit> = [(e.request_id.clone(), e.clone())].into();
        let r = request("e1", "e1", "p1", Some(e_run), ew.max(10));
        let orig = splice::verdict(&e, q1, &Overlap::new(PriorIndex::empty()), 200, &chat_body(&prose(&ALT, 21, 1, ew)));
        assert_eq!(orig["accepted"], true, "{orig}");
        let raw = orig["text"].as_str().unwrap();
        let mut want = orig.clone();
        form_post::spliced_post(&on, &r, q1, &mut want);
        let got = reverdict_spliced(&NotV2, &r, q1, raw, &orig);
        assert_eq!(got, want);
        assert!(got.get("raw_text").is_none(), "{got}");
        let sp = form("span_select2").with_items(Vec::new(), &HashMap::new()).unwrap().with_edits(&edits);
        assert_eq!(reverdict_spliced(&sp, &r, q1, raw, &orig)["raw_text"].as_str(), Some(raw));
    }

    fn sparent(id: &str, domain: &str, text: &str) -> splice::Parent {
        splice::Parent {
            id: id.into(),
            split: "train".into(),
            domain: domain.into(),
            genre: None,
            source_group: format!("g-{id}"),
            writer_key: None,
            cohort_id: "c".into(),
            provenance_sha256: None,
            text: text.into(),
            text_sha256: sha(text.as_bytes()),
            a_tokens: text::words(text),
            words: text::words(text),
        }
    }

    fn sitem(id: &str, population: &str, parent: &splice::Parent, edits: Vec<splice::Edit>) -> splice::Item {
        splice::Item {
            item_id: id.into(),
            population: population.into(),
            split: "train".into(),
            domain: parent.domain.clone(),
            parent_id: parent.id.clone(),
            parent_text_sha256: parent.text_sha256.clone(),
            source_group: parent.source_group.clone(),
            writer_key: None,
            variant: 0,
            order: 0,
            order_t: 0.5,
            policy: "one_sentence".into(),
            policy_requested: "one_sentence".into(),
            policy_fallback_steps: 0,
            generators: vec!["g".into()],
            two_generators: false,
            voice_scope: "document".into(),
            span_half: None,
            edits,
        }
    }

    #[test]
    fn spliced_batch_reads_one_plan_population_and_reverdicts_it() {
        let ps = parents();
        let (q1, q2) = (ps["p1"].clone(), ps["p2"].clone());
        let (pa, pb) = (sparent("p1", "fiction", &q1), sparent("p2", "news", &q2));
        let sw = |sp: (usize, usize)| text::words(&q1[sp.0..sp.1]);
        let e_run = span_of(&q1, 14);
        let e_fold = span_of(&q1, 10);
        let para_end = span_of(&q1, 5).1;
        let e1 = sedit("e1", "run", e_run, None, sw(e_run));
        let e2 = sedit("e2", "insert", (para_end, para_end), Some(" "), 0);
        let e3 = sedit("e3", "run", e_fold, None, sw(e_fold));
        let s4 = span_of(&q2, 2);
        let e4 = sedit("e4", "run", s4, None, text::words(&q2[s4.0..s4.1]));
        let items = vec![
            sitem("s-two", "train", &pa, vec![e1.clone(), e2.clone()]),
            sitem("s-echo", "train", &pa, vec![e3.clone()]),
            sitem("s-dev", "dev_iter_span_eval", &pb, vec![e4.clone()]),
        ];
        let replies: HashMap<&str, String> = [
            ("e1", prose(&ALT, 21, 1, sw(e_run))),
            ("e2", prose(&ALT, 22, 1, 12)),
            ("e3", q1[e_fold.0..e_fold.1].replacen(' ', "  ", 1)),
            ("e4", prose(&ALT, 23, 1, 12)),
        ]
        .into();
        let dir = tempfile::tempdir().unwrap();
        let pdir = dir.path().join("plan");
        fs::create_dir_all(pdir.join("requests")).unwrap();
        let ib = jsonl_bytes(&items.iter().map(|i| serde_json::to_value(i).unwrap()).collect::<Vec<_>>()).unwrap();
        let pbytes = jsonl_bytes(&[serde_json::to_value(&pa).unwrap(), serde_json::to_value(&pb).unwrap()]).unwrap();
        fs::write(pdir.join("items.jsonl"), &ib).unwrap();
        fs::write(pdir.join("parents.jsonl"), &pbytes).unwrap();
        let mut reqs = Vec::new();
        for it in &items {
            for e in &it.edits {
                let mut r = request(&e.request_id, &it.item_id, &it.parent_id, Some(e.parent_span), e.target_words);
                r.split = if it.population == "train" { "train".into() } else { "dev_iter".into() };
                reqs.push(r);
            }
        }
        let rb = jsonl_bytes(&reqs.iter().map(|r| serde_json::to_value(r).unwrap()).collect::<Vec<_>>()).unwrap();
        fs::write(pdir.join("requests/g.jsonl"), &rb).unwrap();
        fs::write(pdir.join("plan.json"), serde_json::to_vec(&json!({"schema": splice::PLAN_SCHEMA, "items": {"sha256": sha(&ib)}, "parents": {"sha256": sha(&pbytes)},
            "request_files": {"g": {"sha256": sha(&rb)}}})).unwrap()).unwrap();
        let ov = Overlap::new(PriorIndex::empty());
        let edits: HashMap<String, splice::Edit> = items.iter().flat_map(|i| i.edits.iter().map(|e| (e.request_id.clone(), e.clone()))).collect();
        let parent_of: HashMap<&str, &str> = [("e1", q1.as_str()), ("e2", q1.as_str()), ("e3", q1.as_str()), ("e4", q2.as_str())].into();
        let vf = |r: &Request, parsed: &Value| splice::verdict(&edits[&r.request_id], parent_of[r.request_id.as_str()], &ov, 200, parsed);
        let mut lines_v = Vec::new();
        for r in &reqs {
            lines_v.extend(lines_of(r, &[(0, replies[r.request_id.as_str()].as_str())], &vf));
        }
        let runs = dir.path().join("runs");
        write_journal(&runs, "g", "journal-1-w0.jsonl", &lines_v);
        // the batch is one plan population: its prior holds that population's parents only
        let b = spliced_batch(&pdir, "train", "span_select2").unwrap();
        assert_eq!(b.name, "population-train");
        assert_eq!(b.form.record(), FormPost::new("span_select2", [("p1", "fiction", q1.as_str())]).unwrap().record());
        assert_eq!(b.items.len(), 2);
        let out = dir.path().join("out");
        let sum = run_batch(&b, &runs, &out).unwrap();
        assert_eq!(sum["plan_kind"], "spliced");
        // the dev_iter population's journal lines are another split: skipped before parsing
        assert_eq!(sum["counts"]["journal_lines_outside_batch"], 0);
        assert_eq!(sum["counts"]["journal_lines_other_splits_skipped_unparsed"], json!({"dev_iter": 2}));
        let recs: BTreeMap<String, Value> = lines(&out.join("reverdict-population-train.jsonl")).into_iter().map(|r| (r["item_id"].as_str().unwrap().to_string(), r)).collect();
        let two = &recs["s-two"];
        assert_eq!(two["pass"], true, "{two}");
        assert_eq!((two["requests"][0]["item_check_last"].as_bool(), two["requests"][1]["item_check_last"].as_bool()), (Some(false), Some(true)));
        assert_eq!(recs["s-echo"]["pass"], false);
        assert!(recs["s-echo"]["reasons"].to_string().contains("echo_span_whitespace_folded_identical"), "{}", recs["s-echo"]);
        let plan = lines(&out.join("regeneration-plan-population-train.jsonl"));
        assert_eq!(plan.len(), 1);
        assert_eq!((plan[0]["request_id"].as_str(), plan[0]["action"].as_str(), plan[0]["batch"].as_str()), (Some("e3"), Some("regenerate"), Some("population-train")));
        // the binding a spliced profile v2 run of this population checks
        let bind = form_post::binding_path(&out, "population-train");
        form_post::check_binding(&bind, Some(&b.form.record()), true).unwrap();
        // a local plan's loader refuses a spliced plan, and the spliced loader a population with no item
        let tok = tokenizer();
        assert!(local_batch(&pdir, "train", "train2", &tok).err().unwrap().to_string().contains("spliced plan"));
        assert!(spliced_batch(&pdir, "test", "span_select2").is_err());
    }

    #[test]
    fn run_batch_reverdicts_journals_and_plans_by_request_id() {
        let fx = fixture();
        let tok = tokenizer();
        let b = batch(&fx, &tok);
        let runs = fx.dir.path().join("runs");
        let out = fx.dir.path().join("out");
        let sum = run_batch(&b, &runs, &out).unwrap();
        let c = &sum["counts"];
        assert_eq!(c["items"], 7);
        assert_eq!(c["items_complete_in_journals"], 6);
        // the torn last line names no split in its bytes, and the dev_iter request's lines are another split: neither is parsed
        assert_eq!((c["torn_lines"].as_u64(), c["journal_lines_without_a_readable_split_skipped_unparsed"].as_u64()), (Some(0), Some(1)));
        assert_eq!(c["journal_lines_other_splits_skipped_unparsed"], json!({"dev_iter": 2}));
        assert_eq!(c["journal_lines_outside_batch"], 0);
        assert_eq!(c["superseded_accepted_responses"], 0);
        assert_eq!(c["raw_body_check"], json!({"equal": 8}));
        assert_eq!(sum["model_calls"], NO_MODEL_CALL);
        // per item
        let recs: BTreeMap<String, Value> = lines(&out.join("reverdict-split-train.jsonl")).into_iter().map(|r| (r["item_id"].as_str().unwrap().to_string(), r)).collect();
        assert_eq!(recs["i-m-ok"]["pass"], true, "{}", recs["i-m-ok"]);
        assert_eq!(recs["i-x"]["pass"], true, "{}", recs["i-x"]);
        for (id, want) in [("i-m-echo", "echo_mirror_byte_identical"), ("i-m-late", "echo_mirror_byte_identical"), ("i-s-list", "form_line_list_like"), ("i-y", "echo_span_whitespace_folded_identical")] {
            assert_eq!(recs[id]["pass"], false, "{id}");
            assert!(recs[id]["reasons"].to_string().contains(want), "{id}: {}", recs[id]["reasons"]);
        }
        assert_eq!((recs["i-inc"]["complete_in_journals"].as_bool(), recs["i-inc"]["action"].is_null()), (Some(false), true));
        // the item rule: the earlier request of a passing two-span item gets its own rules, the last the item check
        let x = &recs["i-x"]["requests"];
        assert_eq!((x[0]["item_check_last"].as_bool(), x[1]["item_check_last"].as_bool()), (Some(false), Some(true)));
        // an echo on the first span rejects it; the second, judged with the first pending, gets its own rules and passes
        let y = &recs["i-y"]["requests"];
        assert_eq!((y[0]["accepted"].as_bool(), y[1]["accepted"].as_bool()), (Some(false), Some(true)));
        assert_eq!(y[1]["item_check_last"], false);
        // the plan: keyed by request id (sorted, unique), attempts counted from the first
        let plan = lines(&out.join("regeneration-plan-split-train.jsonl"));
        let ids: Vec<&str> = plan.iter().map(|r| r["request_id"].as_str().unwrap()).collect();
        assert_eq!(ids, vec!["r-m-echo", "r-m-late", "r-s-list", "r-y-a"]);
        let by: HashMap<&str, &Value> = plan.iter().map(|r| (r["request_id"].as_str().unwrap(), r)).collect();
        // a mirror is regenerated through B5b's new request (mirror_target_words_v2), not by resume
        assert_eq!((by["r-m-echo"]["attempts_used"].as_u64(), by["r-m-echo"]["attempts_remaining"].as_u64(), by["r-m-echo"]["action"].as_str()), (Some(1), Some(1), Some("regenerate_mirror")));
        assert_eq!((by["r-s-list"]["action"].as_str(), by["r-y-a"]["action"].as_str()), (Some("regenerate"), Some("regenerate")));
        assert_eq!((by["r-m-late"]["attempts_used"].as_u64(), by["r-m-late"]["attempts_remaining"].as_u64(), by["r-m-late"]["action"].as_str()), (Some(2), Some(0), Some("drop_out_of_attempts")));
        assert_eq!(by["r-y-a"]["item_id"], "i-y");
        assert_eq!(c["fail_items_by_action"], json!({"drop_out_of_attempts": 1, "regenerate": 2, "regenerate_mirror": 1}));
        // the D4 resume's request order lists the regenerate items only, in plan order
        assert_eq!(fs::read_to_string(out.join("regenerate-order-g.txt")).unwrap(), "i-s-list\ni-y\n");
        assert_eq!(sum["outputs"]["request_orders"]["g"]["items"], 2);
        // the per-request records carry the full text-free profile record (amendment 14 item 14.1's dry-run fields)
        for k in ["transfer", "form_reasons", "unlicensed_reasons", "would_be_unlicensed", "remeasure", "echo"] {
            assert!(recs["i-m-ok"]["requests"][0]["profile_record"].get(k).is_some(), "{k}");
        }
        // the journal: every batch line in input order, carried lines byte for byte, re-verdicted ones with raw_text and a renorm record
        let input = fs::read_to_string(runs.join("g/journal-1-w0.jsonl")).unwrap();
        let kept: Vec<&str> = input.lines().filter(|l| serde_json::from_str::<Value>(l).is_ok_and(|v| v["split"] == "train")).collect();
        let output = fs::read_to_string(out.join("g").join(JOURNAL_NAME)).unwrap();
        let outl: Vec<&str> = output.lines().collect();
        assert_eq!(outl.len(), kept.len());
        let mut re = 0;
        for (la, lb) in kept.iter().zip(&outl) {
            let (va, vb): (Value, Value) = (serde_json::from_str(la).unwrap(), serde_json::from_str(lb).unwrap());
            if vb.get("renorm").is_some() {
                re += 1;
                assert_eq!(va["verdict"]["accepted"], true);
                assert_eq!(vb["verdict"]["raw_text"], va["verdict"]["text"]);
                assert_eq!(vb["renorm"]["raw_response_sha256"].as_str(), Some(sha(va["verdict"]["text"].as_str().unwrap().as_bytes()).as_str()));
                assert_eq!(vb["verdict"]["diag"]["post"]["prior_sha256"].as_str(), Some(b.form.prior_sha256()));
                let mut x = vb.clone();
                x.as_object_mut().unwrap().remove("renorm");
                x["verdict"] = va["verdict"].clone();
                assert_eq!(x, va);
            } else {
                assert_eq!(la, lb);
            }
        }
        assert_eq!(re, 8);
        // the output resumes: rejected requests are pending with their remaining attempts
        let st = generation::load_states(&[out.join("g")]).unwrap();
        assert!(!st["r-m-echo"].done() && st["r-m-late"].done() && st["r-m-ok"].done() && st["r-m-ok"].accepted().is_some());
        assert!(st["r-x-a"].accepted().unwrap()["verdict"]["raw_text"].is_string());
        // the binding and prior record are the batch's profile record, mode 600
        let bind: Value = serde_json::from_slice(&fs::read(out.join("form-profile-split-train.json")).unwrap()).unwrap();
        assert_eq!(bind["profile"], b.form.record());
        form_post::check_binding(&out.join("form-profile-split-train.json"), Some(&b.form.record()), true).unwrap();
        assert!(form_post::check_binding(&out.join("form-profile-split-train.json"), Some(&form("cal2_t").record()), true).is_err());
        assert_eq!(sha(&fs::read(out.join(format!("form-prior-{}.json", b.form.prior_sha256()))).unwrap()), b.form.prior_sha256());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            for (k, _) in tree(&out) {
                assert_eq!(fs::metadata(out.join(&k)).unwrap().permissions().mode() & 0o777, 0o600, "{k}");
            }
            assert_eq!(fs::metadata(&out).unwrap().permissions().mode() & 0o777, 0o700);
        }
        // records hold no text
        let ps = parents();
        for f in ["reverdict-split-train.jsonl", "regeneration-plan-split-train.jsonl", "RENORM-split-train.json"] {
            let s = fs::read_to_string(out.join(f)).unwrap();
            for t in ps.values() {
                for x in text::sentences(t) {
                    assert!(!s.contains(&t[x.start..x.end]), "{f}");
                }
            }
            assert!(!s.contains("Acorn bramble canyon"), "{f}");
        }
        // a second run refuses the existing directory
        assert!(run_batch(&b, &runs, &out).unwrap_err().to_string().contains("exists"));
    }

    #[test]
    fn several_accepted_responses_keep_the_earliest_and_reject_the_rest() {
        let ps = parents();
        let q1 = &ps["p1"];
        let w1 = text::words(q1);
        let off = ctx(None);
        let vf = |r: &Request, parsed: &Value| generation::verdict(&off, r, 200, parsed);
        // an echo accepted with the profile off, then a fresh mirror also accepted (two runs on one request)
        let echo_first = request("r-two-echo", "i-two-echo", "p1", None, w1);
        // two fresh mirrors, both accepted
        let ok_first = request("r-two-ok", "i-two-ok", "p1", None, w1);
        let (fa, fb) = (prose(&ALT, 71, 18, 11), prose(&ALT, 72, 18, 11));
        let dir = tempfile::tempdir().unwrap();
        let runs = dir.path().join("runs");
        let mut g = lines_of(&echo_first, &[(0, q1), (1, &fa)], &vf);
        g.extend(lines_of(&ok_first, &[(0, &fa), (1, &fb)], &vf));
        assert_eq!(g.iter().filter(|l| l["kind"] == "response" && l["verdict"]["accepted"] == true).count(), 4);
        write_journal(&runs, "g", "journal-1-w0.jsonl", &g);
        let tok = tokenizer();
        let b = Batch {
            kind: PlanKind::Local,
            name: "split-train".into(),
            form: form("train2"),
            requests: [&echo_first, &ok_first].iter().map(|r| (r.request_id.clone(), (*r).clone())).collect(),
            items: vec![ritem("i-two-echo", "p1", "mirror", &[&echo_first]), ritem("i-two-ok", "p1", "mirror", &[&ok_first])],
            parents: parents(),
            tokenizer: Some(&tok),
            inputs: json!({"fixture": true}),
            split: "train".into(),
            skipped: Skipped::new(),
            records: None,
        };
        let out = dir.path().join("out");
        let sum = run_batch(&b, &runs, &out).unwrap();
        assert_eq!(sum["counts"]["superseded_accepted_responses"], 2);
        let recs: BTreeMap<String, Value> = lines(&out.join("reverdict-split-train.jsonl")).into_iter().map(|r| (r["item_id"].as_str().unwrap().to_string(), r)).collect();
        // the earliest accepted response is the record's, as finalize reads it
        assert_eq!((recs["i-two-echo"]["pass"].as_bool(), recs["i-two-echo"]["requests"][0]["attempt"].as_u64()), (Some(false), Some(0)));
        assert_eq!((recs["i-two-ok"]["pass"].as_bool(), recs["i-two-ok"]["requests"][0]["attempt"].as_u64()), (Some(true), Some(0)));
        let plan = lines(&out.join("regeneration-plan-split-train.jsonl"));
        assert_eq!(plan.len(), 1);
        assert_eq!((plan[0]["request_id"].as_str(), plan[0]["action"].as_str()), (Some("r-two-echo"), Some("drop_out_of_attempts")));
        // the output journal agrees with the plan: no later accepted response survives
        let outl = lines(&out.join("g").join(JOURNAL_NAME));
        let later: Vec<&Value> = outl.iter().filter(|l| l["kind"] == "response" && l["attempt"] == 1).collect();
        assert_eq!(later.len(), 2);
        for l in later {
            assert_eq!((l["verdict"]["accepted"].as_bool(), l["verdict"]["reasons"].clone()), (Some(false), json!([SUPERSEDED_REASON])), "{l}");
            assert_eq!(l["renorm"]["original_accepted"], true);
        }
        let st = generation::load_states(&[out.join("g")]).unwrap();
        assert!(st["r-two-echo"].accepted().is_none() && st["r-two-echo"].done());
        let acc = st["r-two-ok"].accepted().unwrap();
        assert_eq!((acc["attempt"].as_u64(), acc["renorm"]["schema"].as_str()), (Some(0), Some(RESPONSE_RENORM_SCHEMA)));
    }

    #[test]
    fn journals_and_plans_of_other_splits_are_read_past_unparsed() {
        assert!(refused_split("dev_select") && refused_split("dev-select") && refused_split("test") && !refused_split("dev_iter") && !refused_split("train"));
        assert_eq!(key_values(br#"{"a":1,"split" : "train","x":{"split":"train"}}"#, "split"), vec![Some(&b"train"[..]), Some(&b"train"[..])]);
        assert_eq!(line_split(br#"{"split":"train","x":{"split":"cal"}}"#, "split"), LineSplit::Unreadable);
        assert_eq!(line_split(br#"{"t":"the \"split\":\"x\" text","split":"train"}"#, "split"), LineSplit::One("train".into()));
        assert_eq!(line_split(br#"{"split":3}"#, "split"), LineSplit::Unreadable);
        assert_eq!(line_split(br#"{"t":"split"}"#, "split"), LineSplit::Missing);
        // a fixture's journal with deliberately unparsable dev_select, test and torn lines
        let fx = fixture();
        let tok = tokenizer();
        let b = batch(&fx, &tok);
        let runs = fx.dir.path().join("runs");
        let jp = runs.join("g/journal-1-w0.jsonl");
        let mut bytes = fs::read(&jp).unwrap();
        bytes.extend_from_slice(b"\n{\"kind\":\"response\",\"request_id\":\"r-m-ok\",\"split\":\"dev_select\",\"verdict\":{\x00 never parsed\n");
        bytes.extend_from_slice(b"{\"kind\":\"response\", \"split\": \"test\", broken\n");
        fs::write(&jp, &bytes).unwrap();
        let out = fx.dir.path().join("out");
        let sum = run_batch(&b, &runs, &out).unwrap();
        assert_eq!(sum["counts"]["journal_lines_other_splits_skipped_unparsed"], json!({"dev_iter": 2, "dev_select": 1, "test": 1}));
        assert_eq!(sum["counts"]["journal_lines_without_a_readable_split_skipped_unparsed"], 1);
        // a batch of a refused split never starts
        let mut bad = batch(&fx, &tok);
        bad.split = "dev_select".into();
        assert!(run_batch(&bad, &runs, &fx.dir.path().join("o2")).unwrap_err().to_string().contains("refused"));
        assert!(local_batch(fx.dir.path(), "dev-select", "train2", &tok).err().unwrap().to_string().contains("refused"));
        assert!(local_batch(fx.dir.path(), "test", "train2", &tok).err().unwrap().to_string().contains("refused"));
        // a plan file line whose split cannot be read from its bytes refuses the file by line number
        let f = fx.dir.path().join("items.jsonl");
        fs::write(&f, b"{\"split\":\"train\"}\n{\"split\":\"dev\\u005fselect\"}\n").unwrap();
        let mut sk = Skipped::new();
        let e = read_split_lines(&f, "split", &|s| s == "train", "items", &mut sk).unwrap_err().to_string();
        assert!(e.contains("line 2") && e.contains("refused before any parse"), "{e}");
    }

    #[test]
    fn r0_records_restrict_the_regeneration_plan() {
        let fx = fixture();
        let tok = tokenizer();
        let mut b = batch(&fx, &tok);
        // R0's records: two of the fixture's items and one id of another plan
        b.records = Some(["i-m-ok", "i-y", "other-plan-item"].iter().map(|x| x.to_string()).collect());
        let runs = fx.dir.path().join("runs");
        let out = fx.dir.path().join("out");
        let sum = run_batch(&b, &runs, &out).unwrap();
        assert_eq!(sum["counts"]["r0_records"], json!({"listed": 3, "items_of_the_batch": 2, "not_items_of_the_batch": 1}));
        assert_eq!((sum["counts"]["items_complete_in_journals"].as_u64(), sum["counts"]["items_fail"].as_u64()), (Some(2), Some(1)));
        let plan = lines(&out.join("regeneration-plan-split-train.jsonl"));
        assert_eq!(plan.iter().map(|r| r["request_id"].as_str().unwrap()).collect::<Vec<_>>(), vec!["r-y-a"]);
        assert_eq!(fs::read_to_string(out.join("regenerate-order-g.txt")).unwrap(), "i-y\n");
        let recs: BTreeMap<String, Value> = lines(&out.join("reverdict-split-train.jsonl")).into_iter().map(|r| (r["item_id"].as_str().unwrap().to_string(), r)).collect();
        // items outside R0's records are re-verdicted (the journals stay whole) but never planned
        assert_eq!((recs["i-m-echo"]["r0_record"].as_bool(), recs["i-m-echo"]["pass"].as_bool(), recs["i-m-echo"]["action"].is_null()), (Some(false), Some(false), true));
        assert_eq!(recs["i-y"]["r0_record"], true);
        let p = fx.dir.path().join("records.txt");
        fs::write(&p, "i-m-ok\n\ni-y\n").unwrap();
        let (ids, h) = read_records(&p).unwrap();
        assert_eq!((ids.len(), h), (2, sha(b"i-m-ok\n\ni-y\n")));
    }

    #[test]
    fn run_batch_is_deterministic() {
        let fx = fixture();
        let tok = tokenizer();
        let runs = fx.dir.path().join("runs");
        let (a, b) = (fx.dir.path().join("a"), fx.dir.path().join("b"));
        run_batch(&batch(&fx, &tok), &runs, &a).unwrap();
        // the same inputs, items and requests in another order
        let mut fx2 = batch(&fx, &tok);
        fx2.form = {
            let ps = parents();
            FormPost::new("train2", [("p2", "news", ps["p2"].as_str()), ("p1", "fiction", ps["p1"].as_str())]).unwrap()
        };
        run_batch(&fx2, &runs, &b).unwrap();
        let (ta, tb) = (tree(&a), tree(&b));
        assert_eq!(ta.keys().collect::<Vec<_>>(), tb.keys().collect::<Vec<_>>());
        // the summary names no output path, so every file is the same bytes
        for (k, v) in &ta {
            assert_eq!(v, &tb[k], "{k}");
        }
    }

    #[test]
    fn post_processed_journals_read_raw_text_with_its_digest() {
        let fx = fixture();
        let tok = tokenizer();
        let b = batch(&fx, &tok);
        let runs = fx.dir.path().join("runs");
        let first = fx.dir.path().join("first");
        run_batch(&b, &runs, &first).unwrap();
        // re-verdicting a re-verdict journal reads raw_text, so its verdicts are the same
        let second = fx.dir.path().join("second");
        run_batch(&b, &first, &second).unwrap();
        let v1 = lines(&first.join("reverdict-split-train.jsonl"));
        let v2 = lines(&second.join("reverdict-split-train.jsonl"));
        for (x, y) in v1.iter().zip(&v2) {
            assert_eq!(x["pass"], y["pass"], "{}", x["item_id"]);
            for (rx, ry) in x["requests"].as_array().unwrap().iter().zip(y["requests"].as_array().unwrap()) {
                if ry["accepted_in_journals"] != true {
                    // never accepted, or rejected by the first re-verdict: not accepted in its journal
                    assert_ne!(rx["accepted"], true);
                    continue;
                }
                assert_eq!((&rx["raw_response_sha256"], &rx["text_sha256"], &rx["reasons"]), (&ry["raw_response_sha256"], &ry["text_sha256"], &ry["reasons"]));
            }
        }
        // a raw_text that does not match its digest is refused
        let jp = first.join("g").join(JOURNAL_NAME);
        let s = fs::read_to_string(&jp).unwrap();
        let mut out_lines: Vec<String> = Vec::new();
        let mut done = false;
        for l in s.lines() {
            let mut v: Value = serde_json::from_str(l).unwrap();
            if !done && v.get("renorm").is_some() && v["verdict"]["accepted"] == true {
                v["verdict"]["raw_text"] = json!("tampered");
                done = true;
            }
            out_lines.push(serde_json::to_string(&v).unwrap());
        }
        assert!(done);
        fs::write(&jp, out_lines.join("\n") + "\n").unwrap();
        let e = run_batch(&b, &first, &fx.dir.path().join("third")).unwrap_err().to_string();
        assert!(e.contains("raw_response_sha256"), "{e}");
    }

    fn row(id: &str, kind: &str, cohort: &str, origin: &str, group: &str, domain: &str, text: &str) -> Value {
        let spans = if origin == "model_only" { json!([[0, text.len()]]) } else { json!([]) };
        json!({"id": id, "split": "train", "text": text, "text_sha256": sha(text.as_bytes()), "origin": origin, "source_kind": kind,
            "cohort_id": cohort, "source_group": group, "domain": domain, "ai_byte_spans": spans, "weight": 1.0})
    }

    #[test]
    fn donors_are_same_domain_keyed_and_order_free() {
        let pool: Vec<Donor> = (0..30).map(|i| Donor { id: format!("h{i}"), domain: if i % 3 == 0 { "news".into() } else { "fiction".into() }, text: format!("text {i}") }).collect();
        let mut rev = pool.clone();
        rev.reverse();
        let mut picks = BTreeSet::new();
        for k in 0..40 {
            let rid = format!("stage3:row-{k}");
            let d = pick_donor(&rid, "fiction", &pool).unwrap();
            assert_eq!(d.domain, "fiction");
            // the smallest plan key among same-domain candidates, whatever the pool order
            let want = pool.iter().filter(|x| x.domain == "fiction").min_by_key(|x| donor_key(&rid, &x.id)).unwrap();
            assert_eq!(d, want);
            assert_eq!(pick_donor(&rid, "fiction", &rev).unwrap(), d);
            // other domains' candidates never change the draw
            let fiction_only: Vec<Donor> = pool.iter().filter(|x| x.domain == "fiction").cloned().collect();
            assert_eq!(pick_donor(&rid, "fiction", &fiction_only).unwrap(), d);
            picks.insert(d.id.clone());
        }
        assert!(picks.len() > 5, "rows draw different donors: {picks:?}");
        assert_eq!(donor_key("a", "b"), sha(b"slop_ninja_a_comp_r2_plan_v1:layout_donor:a:b"));
        assert!(pick_donor("x", "legal", &pool).is_none());
    }

    /// A RAID pool document (`slop_ninja_a_pool_document_v1`'s fields that
    /// `raid_humans` reads).
    fn raid_doc(id: &str, origin: &str, group: &str, attack: &str, text: &str) -> Value {
        json!({"schema": "slop_ninja_a_pool_document_v1", "id": id, "origin": origin, "source_group": group, "domain": "raid_books",
            "cohort_id": "raid-v1-paired", "intended_use": "train_only", "split_assigned": false, "text": text, "text_sha256": sha(text.as_bytes()),
            "raid": {"attack": {"name": attack, "attacked": attack != "none"}}})
    }

    /// A mixed row of `parent` with sentence `i` replaced by `with`: the row
    /// text and its one AI span.
    fn mixed_of(parent: &str, i: usize, with: &str) -> (String, (usize, usize)) {
        let s = span_of(parent, i);
        let t = format!("{}{}{}", &parent[..s.0], with, &parent[s.1..]);
        let e = t.len() - (parent.len() - s.1);
        (t, (s.0, e))
    }

    fn mixed_row(id: &str, kind: &str, group: &str, domain: &str, text: &str, span: (usize, usize)) -> Value {
        let mut v = row(id, kind, if kind == "hosted_capture" { "frontier-captures-v1" } else { LEGACY }, "mixed", group, domain, text);
        v["ai_byte_spans"] = json!([[span.0, span.1]]);
        v
    }

    #[test]
    fn rows_take_raid_humans_recorded_or_recovered_parents_or_donors() {
        let ps = parents();
        let (q1, q2) = (ps["p1"].clone(), ps["p2"].clone());
        let q3 = format!("{}\n\n{}", prose(&SYN, 91, 6, 11), prose(&SYN, 92, 7, 11));
        let gen1 = prose(&ALT, 41, 17, 11);
        let (mixed, mixed_span) = mixed_of(&q1, 3, &prose(&ALT, 42, 1, 11));
        let (mixed_b, span_b) = mixed_of(&q3, 2, &prose(&ALT, 43, 1, 11));
        let (mixed_z, span_z) = mixed_of(&q3, 4, &prose(&ALT, 44, 1, 11));
        let mut cap = mixed_row("capture:m", "hosted_capture", "g-c", "fiction", &mixed, mixed_span);
        cap["parent_id"] = json!("stage3:p1");
        let mut whole = mixed_row("stage3:mw", "stage3_legacy", "g-w", "fiction", &gen1, (0, gen1.len()));
        whole["ai_byte_spans"] = json!([[0, gen1.len()]]);
        let mut np = row("capture:np", "hosted_capture", "frontier-captures-v1", "model_only", "g-np", "fiction", &gen1);
        np["parent_id"] = json!("stage3:m1");
        let mut gone = row("capture:gone", "hosted_capture", "frontier-captures-v1", "model_only", "g-gone", "fiction", &gen1);
        gone["parent_id"] = json!("stage3:absent");
        let short = prose(&ALT, 51, 5, 12);
        let rows_v = vec![
            row("raid-h1", "external_pool", "raid-v1-paired", "human_only", "raid_source:1", "raid_books", &q2),
            row("raid-g1", "external_pool", "raid-v1-paired", "model_only", "raid_source:1", "raid_books", &gen1),
            row("raid-g2", "external_pool", "raid-v1-paired", "model_only", "raid_source:2", "raid_books", &gen1),
            // an attacked human variant and the unattacked human: the unattacked one is the RAID human
            row("raid-h3a", "external_pool", "raid-v1-paired", "human_only", "raid_source:3", "raid_books", &q1),
            row("raid-h3b", "external_pool", "raid-v1-paired", "human_only", "raid_source:3", "raid_books", &q2),
            row("raid-g3", "external_pool", "raid-v1-paired", "model_only", "raid_source:3", "raid_books", &gen1),
            // only an attacked variant in the bundle; the unattacked human is a pool document
            row("raid-h4", "external_pool", "raid-v1-paired", "human_only", "raid_source:4", "raid_books", &q1),
            row("raid-g4", "external_pool", "raid-v1-human-paraphrase", "model_only", "raid_source:4", "raid_books", &gen1),
            row("raid-g5", "external_pool", "raid-v1-paired", "model_only", "raid_source:5", "raid_books", &gen1),
            row("raid-h7", "external_pool", "raid-v1-paired", "human_only", "raid_source:7", "raid_books", &q3),
            // a RAID echo: the child is its human
            row("raid-g7", "external_pool", "raid-v1-paired", "model_only", "raid_source:7", "raid_books", &q3),
            row("stage3:p1", "stage3_legacy", LEGACY, "human_only", "g-p", "fiction", &q1),
            row("stage3:m1", "stage3_legacy", LEGACY, "model_only", "g-m", "fiction", &gen1),
            row("stage3:m2", "stage3_legacy", LEGACY, "model_only", "g-m2", "poetry", &gen1),
            cap,
            // no recorded parent: the source group's stage-3 human whose bytes equal the row outside its span
            mixed_row("stage3:mx", "stage3_legacy", "g-p", "fiction", &mixed, mixed_span),
            row("stage3:qa", "stage3_legacy", LEGACY, "human_only", "g-q", "fiction", &q2),
            row("stage3:qb", "stage3_legacy", LEGACY, "human_only", "g-q", "fiction", &q3),
            mixed_row("stage3:my", "stage3_legacy", "g-q", "fiction", &mixed_b, span_b),
            mixed_row("stage3:mz", "stage3_legacy", "g-z", "fiction", &mixed_z, span_z),
            whole,
            np,
            gone,
            row("local:1", "local_positive", "primary_mirror", "model_only", "g-l", "fiction", &gen1),
            row("stage3:short", "stage3_legacy", LEGACY, "model_only", "g-s", "fiction", &short),
        ];
        let at = |id: &str| rows_v.iter().position(|v| v["id"] == id).unwrap();
        let rows: Vec<Row> = rows_v.iter().map(|v| Row::from_value(v).unwrap()).collect();
        let docs = vec![
            raid_doc("raid-h1", "human_only", "raid_source:1", "none", &q2),
            raid_doc("raid-g1", "model_only", "raid_source:1", "none", &gen1),
            raid_doc("raid-h3a", "human_only", "raid_source:3", "whitespace", &q1),
            raid_doc("raid-h3b", "human_only", "raid_source:3", "none", &q2),
            raid_doc("raid-h4", "human_only", "raid_source:4", "homoglyph", &q1),
            raid_doc("raid-h4n", "human_only", "raid_source:4", "none", &q3),
            raid_doc("raid-h5a", "human_only", "raid_source:5", "none", &q2),
            raid_doc("raid-h5b", "human_only", "raid_source:5", "none", &q3),
            raid_doc("raid-h7", "human_only", "raid_source:7", "none", &q3),
            raid_doc("raid-h9", "human_only", "raid_source:9", "none", &q2),
        ];
        let dir = tempfile::tempdir().unwrap();
        let docs_p = dir.path().join("raid-documents.jsonl");
        fs::write(&docs_p, jsonl_bytes(&docs).unwrap()).unwrap();
        let groups: BTreeSet<&str> = rows.iter().filter(|r| r.cohort_id.starts_with(RAID_COHORT_PREFIX) && r.origin == "model_only").map(|r| r.source_group.as_str()).collect();
        let raid = raid_humans(&docs_p, &groups).unwrap();
        assert_eq!(raid.iter().map(|h| h.id.as_str()).collect::<Vec<_>>(), vec!["raid-h1", "raid-h3a", "raid-h3b", "raid-h4", "raid-h4n", "raid-h5a", "raid-h5b", "raid-h7"]);
        // donors with their own quote and dash majorities, which a donor row never takes
        let donors: Vec<Donor> = (0..6)
            .map(|k| Donor {
                id: format!("d{k}"),
                domain: "fiction".into(),
                text: format!("{} They said \u{201C}amber\u{201D}\u{2014}twice. {}\n\n{}", prose(&SYN, 60 + k, 4, 11), prose(&SYN, 80 + k, 1, 11), prose(&SYN, 70 + k, 6, 11)),
            })
            .collect();
        let plans = plan_rows(&rows, &raid, &donors);
        let pl = |id: &str| plans[at(id)].clone();
        let drop = |reason| Some(RowPlan::Drop { reason });
        assert_eq!(pl("raid-h1"), None);
        assert_eq!(pl("raid-g1"), Some(RowPlan::Mirror { parent: "raid-h1".into(), source: "raid_human" }));
        assert_eq!(pl("raid-g2"), drop("renorm_raid_human_missing"));
        assert_eq!(pl("raid-g3"), Some(RowPlan::Mirror { parent: "raid-h3b".into(), source: "raid_human" }));
        assert_eq!(pl("raid-g4"), Some(RowPlan::Mirror { parent: "raid-h4n".into(), source: "raid_human" }));
        assert_eq!(pl("raid-g5"), drop("renorm_raid_human_ambiguous"));
        assert_eq!(pl("stage3:m1"), Some(RowPlan::Donor { donor: pick_donor("stage3:m1", "fiction", &donors).unwrap().id.clone() }));
        assert_eq!(pl("stage3:m2"), drop("renorm_donor_missing"));
        let s3 = span_of(&q1, 3);
        assert_eq!(pl("capture:m"), Some(RowPlan::Span { parent: "stage3:p1".into(), parent_span: s3, row_span: mixed_span, source: "recorded_parent" }));
        assert_eq!(pl("stage3:mx"), Some(RowPlan::Span { parent: "stage3:p1".into(), parent_span: s3, row_span: mixed_span, source: "stage3_source_group" }));
        assert_eq!(pl("stage3:my"), Some(RowPlan::Span { parent: "stage3:qb".into(), parent_span: span_of(&q3, 2), row_span: span_b, source: "stage3_source_group" }));
        assert_eq!(pl("stage3:mz"), drop("renorm_stage3_parent_unrecoverable"));
        assert_eq!(pl("stage3:mw"), Some(RowPlan::Donor { donor: pick_donor("stage3:mw", "fiction", &donors).unwrap().id.clone() }));
        assert_eq!(pl("capture:np"), drop("renorm_recorded_parent_not_human"));
        assert_eq!(pl("capture:gone"), drop("renorm_recorded_parent_missing"));
        assert_eq!(pl("local:1"), None);
        assert_eq!(parent_span_of(&q1, &mixed, mixed_span), Some(s3));
        assert_eq!(parent_span_of(&q2, &mixed, mixed_span), None);
        // end to end, twice: deterministic outputs, receipts for drops, normalised text only for kept rows
        let rp = dir.path().join("rows.jsonl");
        fs::write(&rp, jsonl_bytes(&rows_v).unwrap()).unwrap();
        let mut dv: Vec<Value> = donors.iter().map(|d| row(&d.id, "human", "h", "human_only", &format!("g-{}", d.id), &d.domain, &d.text)).collect();
        let mut zero = row("dz", "human", "h", "human_only", "g-dz", "fiction", &q2);
        zero["weight"] = json!(0.0);
        dv.push(zero);
        let dp = dir.path().join("donors.jsonl");
        fs::write(&dp, jsonl_bytes(&dv).unwrap()).unwrap();
        let sum = renorm_rows(&rp, Some(&dp), Some(&docs_p), "train2", &dir.path().join("o1"), false).unwrap();
        renorm_rows(&rp, Some(&dp), Some(&docs_p), "train2", &dir.path().join("o2"), false).unwrap();
        assert_eq!(tree(&dir.path().join("o1")), tree(&dir.path().join("o2")));
        // the plan alone: counts by class and layout source or drop reason, nothing else written
        let po = renorm_rows(&rp, Some(&dp), Some(&docs_p), "train2", &dir.path().join("op"), true).unwrap();
        assert_eq!(po["planned"], 16);
        assert_eq!(po["planned_by_class"]["raid"], json!({"raid_human": 3, "raid_human:pool_document": 1, "renorm_raid_human_ambiguous": 1, "renorm_raid_human_missing": 1}));
        assert_eq!(po["planned_by_class"]["stage3_legacy:span"], json!({"renorm_stage3_parent_unrecoverable": 1, "stage3_source_group": 2}));
        assert_eq!(po["planned_by_class"]["hosted_capture:whole"], json!({"renorm_recorded_parent_missing": 1, "renorm_recorded_parent_not_human": 1}));
        assert_eq!(tree(&dir.path().join("op")).keys().collect::<Vec<_>>(), vec!["ROWS-PLAN.json"]);
        assert_eq!(sum["inputs"]["donors"]["weight_not_positive_excluded"], 1);
        assert_eq!(sum["inputs"]["raid_documents"]["of_them_bundle_rows"], 5);
        assert_eq!(sum["counts"]["planned"], 16);
        assert_eq!(sum["counts"]["non_human_rows_without_a_plan_by_source_kind"], json!({"local_positive": 1}));
        let rv: HashMap<String, Value> = lines(&dir.path().join("o1/rows-reverdict.jsonl")).into_iter().map(|r| (r["id"].as_str().unwrap().to_string(), r)).collect();
        for id in ["raid-g2", "raid-g5", "raid-g7", "stage3:m2", "stage3:mz", "capture:np", "capture:gone", "stage3:short"] {
            assert_eq!(rv[id]["action"], "drop_with_receipt", "{id}");
        }
        assert_eq!((rv["raid-g1"]["layout_source"]["id"].as_str(), rv["raid-g1"]["layout_source"]["bundle_row"].as_bool()), (Some("raid-h1"), Some(true)));
        assert_eq!((rv["raid-g4"]["layout_source"]["id"].as_str(), rv["raid-g4"]["layout_source"]["bundle_row"].as_bool()), (Some("raid-h4n"), Some(false)));
        assert_eq!(rv["stage3:mx"]["layout_source"]["kind"], "stage3_source_group");
        assert_eq!(rv["stage3:m1"]["layout_source"]["kind"], "layout_donor");
        assert_eq!(rv["stage3:m1"]["record"]["echo"]["applied"], false);
        // every RAID child gets the echo screen: an echo of the RAID human is rejected by it
        let e7 = &rv["raid-g7"];
        assert_eq!((e7["record"]["echo"]["edits"].as_u64(), e7["record"]["echo"]["rejected"].as_bool()), (Some(1), Some(true)), "{e7}");
        assert!(e7["reasons"].to_string().contains("echo_mirror_byte_identical"), "{}", e7["reasons"]);
        // the window runs on every laid-out row: a short donor row fails it and nothing else
        assert_eq!(rv["stage3:short"]["reasons"], json!(["final_document_below_150_words"]), "{}", rv["stage3:short"]);
        // a donor transfers layout only: no convention comes from it as a parent
        for (k, c) in rv["stage3:m1"]["record"]["conventions"].as_object().unwrap() {
            assert!(c["source"].as_str().unwrap().starts_with("keyed_prior") || c["source"] == "none", "{k}: {c}");
        }
        // the batch's prior holds its parents only (no donor), each in its own domain
        let pr: Value = serde_json::from_slice(&fs::read(dir.path().join("o1").join(sum["outputs"]["prior"].as_str().unwrap())).unwrap()).unwrap();
        assert_eq!(sha(&fs::read(dir.path().join("o1").join(sum["outputs"]["prior"].as_str().unwrap())).unwrap()), sum["prior_sha256"].as_str().unwrap());
        assert_eq!(pr["parents"], sum["counts"]["parents"]);
        assert_eq!(pr["parents_by_domain"], json!({"fiction": 2, "raid_books": 4}));
        assert_eq!(rv["raid-g1"]["record"]["echo"]["id"], "cc_echo_screen_v1");
        let nm = lines(&dir.path().join("o1/rows-normalised.jsonl"));
        let kept: BTreeSet<&str> = rv.values().filter(|r| r["action"] == "keep").map(|r| r["id"].as_str().unwrap()).collect();
        assert_eq!(nm.iter().map(|r| r["id"].as_str().unwrap()).collect::<BTreeSet<_>>(), kept);
        assert!(kept.contains("stage3:mx") && kept.contains("raid-g3"), "{kept:?}");
        for n in &nm {
            let t = n["text"].as_str().unwrap();
            assert_eq!(n["text_sha256"].as_str(), Some(sha(t.as_bytes()).as_str()));
            let sp = &n["ai_byte_spans"][0];
            let (a, b) = (sp[0].as_u64().unwrap() as usize, sp[1].as_u64().unwrap() as usize);
            assert!(b <= t.len() && a < b);
        }
        let s = fs::read_to_string(dir.path().join("o1/rows-reverdict.jsonl")).unwrap();
        for t in [&q1, &q2, &q3, &gen1] {
            for x in text::sentences(t) {
                assert!(!s.contains(&t[x.start..x.end]));
            }
        }
        // the donors default to the rows' own positive-weight humans
        let own = renorm_rows(&rp, None, Some(&docs_p), "train2", &dir.path().join("o4"), false).unwrap();
        assert_eq!((own["inputs"]["donors"]["same_as_rows"].as_bool(), own["inputs"]["donors"]["donors"].as_u64()), (Some(true), Some(8)));
        // RAID rows need the RAID documents, and a document that differs from its bundle row is refused
        assert!(renorm_rows(&rp, Some(&dp), None, "train2", &dir.path().join("o5"), false).unwrap_err().to_string().contains("--raid-documents"));
        let mut bad = docs.clone();
        bad[0] = raid_doc("raid-h1", "human_only", "raid_source:1", "none", &q1);
        fs::write(&docs_p, jsonl_bytes(&bad).unwrap()).unwrap();
        assert!(renorm_rows(&rp, Some(&dp), Some(&docs_p), "train2", &dir.path().join("o6"), false).unwrap_err().to_string().contains("differs from its bundle row"));
    }

    #[test]
    fn rows_draws_are_keyed_by_the_row_id() {
        // three stage-3 parents of one domain: curly and straight double quotes, and none
        let pa = format!("{} She said \u{201C}amber harbor\u{201D} twice. {}\n\n{}", prose(&SYN, 1, 3, 11), prose(&SYN, 2, 2, 11), prose(&SYN, 3, 6, 11));
        let pb = format!("{} He said \"cobalt delta\" twice. {}\n\n{}", prose(&SYN, 4, 3, 11), prose(&SYN, 5, 2, 11), prose(&SYN, 6, 6, 11));
        let pc = format!("{}\n\n{}", prose(&SYN, 7, 6, 11), prose(&SYN, 8, 7, 11));
        let parents: Vec<(&str, &str)> = vec![("fiction", pa.as_str()), ("fiction", pb.as_str()), ("fiction", pc.as_str())];
        let prior = PriorV2::of("train2", parents.into_iter());
        let seed = norm_prior_seed("train2");
        let c = counts(&pc);
        // a row id whose keyed draw differs from the parent id's
        let rid = (0..200)
            .map(|k| format!("stage3:k{k}"))
            .find(|id| json!(resolve_v2(&c, &prior, "fiction", &seed, id))["double"] != json!(resolve_v2(&c, &prior, "fiction", &seed, "stage3:pc"))["double"])
            .expect("the keyed draw varies");
        let (ma, sa) = mixed_of(&pa, 4, &prose(&ALT, 61, 1, 11));
        let (mb, sb) = mixed_of(&pb, 4, &prose(&ALT, 62, 1, 11));
        let (mc, sc) = mixed_of(&pc, 3, "The keeper said \"marsh nettle\" and walked on past the oasis ridge again.");
        let rows_v = vec![
            row("stage3:pa", "stage3_legacy", LEGACY, "human_only", "g-a", "fiction", &pa),
            row("stage3:pb", "stage3_legacy", LEGACY, "human_only", "g-b", "fiction", &pb),
            row("stage3:pc", "stage3_legacy", LEGACY, "human_only", "g-c", "fiction", &pc),
            mixed_row("stage3:ka", "stage3_legacy", "g-a", "fiction", &ma, sa),
            mixed_row("stage3:kb", "stage3_legacy", "g-b", "fiction", &mb, sb),
            mixed_row(&rid, "stage3_legacy", "g-c", "fiction", &mc, sc),
        ];
        let dir = tempfile::tempdir().unwrap();
        let rp = dir.path().join("rows.jsonl");
        fs::write(&rp, jsonl_bytes(&rows_v).unwrap()).unwrap();
        renorm_rows(&rp, None, None, "train2", &dir.path().join("o"), false).unwrap();
        let rv: HashMap<String, Value> = lines(&dir.path().join("o/rows-reverdict.jsonl")).into_iter().map(|r| (r["id"].as_str().unwrap().to_string(), r)).collect();
        let got = &rv[&rid]["record"]["conventions"];
        assert_eq!(got, &json!(resolve_v2(&c, &prior, "fiction", &seed, &rid)), "{got}");
        assert_eq!(got["double"]["source"], "keyed_prior_domain");
        assert_eq!(rv[&rid]["record"]["key"]["value"].as_str(), Some(rid.as_str()));
    }

    #[test]
    fn rows_of_other_splits_are_skipped_before_parsing() {
        let ps = parents();
        let gen1 = prose(&ALT, 41, 17, 11);
        let good = vec![
            row("stage3:p1", "stage3_legacy", LEGACY, "human_only", "g-p", "fiction", &ps["p1"]),
            row("stage3:m1", "stage3_legacy", LEGACY, "model_only", "g-m", "fiction", &gen1),
        ];
        let dir = tempfile::tempdir().unwrap();
        let rp = dir.path().join("rows.jsonl");
        // a v1 bundle's other splits, deliberately unparsable after their split key
        let mut b = jsonl_bytes(&good).unwrap();
        b.extend_from_slice(b"{\"split\":\"dev_select\",\"text\":\"never \x00 parsed\n");
        b.extend_from_slice(b"{\"id\":\"x\", \"split\" : \"test\", broken\n");
        b.extend_from_slice(b"{\"split\":\"dev_iter\",\"oops\n");
        fs::write(&rp, &b).unwrap();
        let sum = renorm_rows(&rp, None, None, "train2", &dir.path().join("o"), false).unwrap();
        assert_eq!(sum["counts"]["lines_skipped_unparsed"], json!({"rows": {"dev_iter": 1, "dev_select": 1, "test": 1}}));
        assert_eq!(sum["counts"]["planned"], 1);
        // a line whose split cannot be read from its bytes refuses the file, by line number
        let mut bad = jsonl_bytes(&good).unwrap();
        bad.extend_from_slice(b"{\"split\":\"dev\\u005fselect\",\"text\":\"x\"}\n");
        fs::write(&rp, &bad).unwrap();
        let e = renorm_rows(&rp, None, None, "train2", &dir.path().join("o2"), false).unwrap_err().to_string();
        assert!(e.contains("line 3") && e.contains("refused before any parse"), "{e}");
        fs::write(&rp, [jsonl_bytes(&good).unwrap(), b"{\"id\":\"no split\"}\n".to_vec()].concat()).unwrap();
        assert!(renorm_rows(&rp, None, None, "train2", &dir.path().join("o3"), false).is_err());
    }

    const LEGACY: &str = "stage3_legacy_train";

    /// A word longer than the budget left after any late column (amendment
    /// 38's first-word case).
    const LONG: &str = "Counterrevolutionaries";

    /// Greedy hard wrap at `w` columns.
    fn wrap_at(p: &str, w: usize) -> String {
        let mut lines: Vec<String> = Vec::new();
        let mut cur = String::new();
        for word in p.split_whitespace() {
            if !cur.is_empty() && cur.chars().count() + 1 + word.chars().count() > w {
                lines.push(std::mem::take(&mut cur));
            }
            if !cur.is_empty() {
                cur.push(' ');
            }
            cur.push_str(word);
        }
        lines.push(cur);
        lines.join("\n")
    }

    /// P3: two paragraphs hard-wrapped at 48 columns, about 190 words.
    fn p3() -> String {
        format!("{}\n\n{}", wrap_at(&prose(&SYN, 101, 8, 12), 48), wrap_at(&prose(&SYN, 102, 8, 12), 48))
    }

    /// Sentences of `p` that start after some text on their line, late
    /// enough that `LONG` has no room there, each on its own line.
    fn late_sentences(p: &str) -> Vec<(usize, usize)> {
        let mut out: Vec<(usize, usize)> = Vec::new();
        for x in text::sentences(p) {
            let ls = p[..x.start].rfind('\n').map_or(0, |k| k + 1);
            let col = p[ls..x.start].chars().count();
            if col > 0 && col + LONG.len() > 48 && out.last().is_none_or(|l| p[l.1..x.start].contains('\n')) {
                out.push((x.start, x.end));
            }
        }
        out
    }

    /// A replacement of `n` words whose first is `LONG`.
    fn long_first(seed: u64, n: usize) -> String {
        let t = prose(&ALT, seed, 1, n - 1);
        format!("{LONG} {}{}", &t[..1].to_lowercase(), &t[1..])
    }

    #[test]
    fn run_batch_counts_each_document_line_split_once() {
        // Amendment 38 item 38.1's totals: an item of two spans whose edits
        // each split a document line. The item's last request is judged on
        // the whole item, so its record holds both edits; the summary counts
        // each application once (the first request's own and the last's own).
        let q3 = p3();
        let ls = late_sentences(&q3);
        assert!(ls.len() >= 2, "{ls:?}");
        let (s_a, s_b) = (ls[0], ls[ls.len() - 1]);
        let sw = |sp: (usize, usize)| text::words(&q3[sp.0..sp.1]);
        let mut ps = parents();
        ps.insert("p3".into(), q3.clone());
        let mut off = ctx(None);
        off.parents = ps.clone();
        let vf = |r: &Request, parsed: &Value| generation::verdict(&off, r, 200, parsed);
        let ma = request("r-z-a", "i-z", "p3", Some(s_a), sw(s_a));
        let mb = request("r-z-b", "i-z", "p3", Some(s_b), sw(s_b));
        let dir = tempfile::tempdir().unwrap();
        let runs = dir.path().join("runs");
        let mut g = lines_of(&ma, &[(0, &long_first(201, sw(s_a)))], &vf);
        g.extend(lines_of(&mb, &[(0, &long_first(202, sw(s_b)))], &vf));
        write_journal(&runs, "g", "journal-1-w0.jsonl", &g);
        let tok = tokenizer();
        let form = FormPost::new("train2", [("p1", "fiction", ps["p1"].as_str()), ("p2", "news", ps["p2"].as_str()), ("p3", "news", q3.as_str())]).unwrap();
        let b = Batch {
            kind: PlanKind::Local,
            name: "split-train".into(),
            form,
            requests: [&ma, &mb].iter().map(|r| (r.request_id.clone(), (*r).clone())).collect(),
            items: vec![ritem("i-z", "p3", "span", &[&ma, &mb])],
            parents: ps,
            tokenizer: Some(&tok),
            inputs: json!({"fixture": true}),
            split: "train".into(),
            skipped: Skipped::new(),
            records: None,
        };
        let out = dir.path().join("out");
        let sum = run_batch(&b, &runs, &out).unwrap();
        let recs = lines(&out.join("reverdict-split-train.jsonl"));
        let q = &recs[0]["requests"];
        let (first, last) = (&q[0]["profile_record"], &q[1]["profile_record"]);
        assert_eq!((first["item_check"]["last"].as_bool(), last["item_check"]["last"].as_bool()), (Some(false), Some(true)), "{}", recs[0]);
        let n = |r: &Value| r["transfer"]["document_line_splits"].as_u64().unwrap_or(0);
        let own_first = n(&first["edits"][0]);
        let (e0, e1) = (n(&last["edits"][0]), n(&last["edits"][1]));
        assert!(own_first >= 1 && e1 >= 1, "both edits split: {own_first} {e1}");
        assert_eq!(e0, own_first, "the last request's record repeats the earlier edit");
        assert_eq!((request_line_splits(first), request_line_splits(last)), (own_first, e1));
        assert_eq!(sum["counts"]["document_line_splits"].as_u64(), Some(own_first + e1));
        assert_eq!(grammar_a_form::profile::document_line_splits(last), e0 + e1, "every edit of the whole-item record");
    }

    #[test]
    fn rows_count_document_line_splits_and_start_spans_after_the_lead() {
        // A stage-3 mixed row whose span starts late on a hard-wrapped line
        // of its recorded human, with a first word that has no room there:
        // amendment 38 breaks before it, the summary counts it, and the
        // normalised AI span starts after the line break and lead (amendment
        // 39 item 39.2), as an item's edit's final span does.
        let q3 = p3();
        let sp = late_sentences(&q3)[0];
        let with = long_first(211, text::words(&q3[sp.0..sp.1]));
        let t = format!("{}{}{}", &q3[..sp.0], with, &q3[sp.1..]);
        let rs = (sp.0, sp.0 + with.len());
        let rows_v = vec![row("stage3:h3", "stage3_legacy", LEGACY, "human_only", "g-3", "fiction", &q3), mixed_row("stage3:m3", "stage3_legacy", "g-3", "fiction", &t, rs)];
        let dir = tempfile::tempdir().unwrap();
        let rp = dir.path().join("rows.jsonl");
        fs::write(&rp, jsonl_bytes(&rows_v).unwrap()).unwrap();
        let sum = renorm_rows(&rp, None, None, "train2", &dir.path().join("o"), false).unwrap();
        let rv = lines(&dir.path().join("o/rows-reverdict.jsonl"));
        assert_eq!(rv.len(), 1);
        let k = rv[0]["record"]["transfer"]["document_line_splits"].as_u64().unwrap_or(0);
        assert!(k >= 1, "{}", rv[0]);
        assert_eq!(sum["counts"]["document_line_splits"].as_u64(), Some(k));
        assert_eq!(rv[0]["action"], "keep", "{}", rv[0]);
        let nm = lines(&dir.path().join("o/rows-normalised.jsonl"));
        let d = nm[0]["text"].as_str().unwrap();
        let a = nm[0]["ai_byte_spans"][0][0].as_u64().unwrap() as usize;
        let lead = &d[sp.0..a];
        assert!(lead.starts_with('\n') && lead.trim().is_empty(), "{lead:?}");
        assert!(d[a..].starts_with(LONG));
        assert_eq!(&d[..sp.0], &q3[..sp.0]);
    }

    #[test]
    fn the_renorm_path_holds_no_http_client() {
        let src = include_str!("renorm.rs");
        let head = &src[..src.find("#[cfg(test)]\nmod tests").unwrap()];
        for x in ["reqwest", "client(", "Ctx", "std::net", "TcpStream", "Command::new", "process::"] {
            assert!(!head.contains(x), "{x}");
        }
    }
}
