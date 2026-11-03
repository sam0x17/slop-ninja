//! `dryrun-count`: the §4.3 dry run's uncounted figures and child-side stop
//! invariants (protocol A2 item 14.1; BUILD-PLAN 12.3, "B23"; ruling W7-R1)
//! for the local, spliced and rows batches, read from the dry run's
//! recorded outputs only. The counting rules are `grammar_a_dryrun`'s; this
//! module reads a batch and gives each child to `Tally::count`.
//!
//! **Inputs** (no re-verdict re-run, no generation, no model call, no keyed
//! draw):
//! * the batch's renorm summary (`RENORM-<batch>.json`, `ROWS-RENORM.json`),
//!   refused unless its SHA-256 is the one given (index 60's, or the one the
//!   dry run's record lists); every file the summary names is checked
//!   against the digest it records before it is read;
//! * the per-request re-verdict records (`reverdict-<batch>.jsonl`,
//!   `rows-reverdict.jsonl`): each child's recorded verdict;
//! * the output journals (`<generator>/journal-renorm.jsonl`): each
//!   re-verdicted record response's laid-out child (`text`), raw child
//!   (`raw_text`) and profile record (`diag.post`, its `conventions`);
//! * the parents, from the batch's recorded inputs (`renorm::local_batch`,
//!   `renorm::spliced_batch`; the rows and RAID documents of the rows
//!   summary): every line's split is read from its bytes before it is
//!   parsed, and only Train lines are parsed (item 20.1);
//! * R0's records, as `DRY-RUN-FIGURES.json` restricts them (its
//!   `inputs_note`): local items with `r0_record`, spliced items of the S0+
//!   snapshot, every planned rows row; R0's Train build rows give each local
//!   child's class and domain.
//!
//! **Children** are the dry run's (`figures.py`): every request of a
//! complete R0 item whose record response the journals hold, every rows row
//! with a transfer record. Each child is re-derived and counted by
//! `Tally::count`; its landed verdict is recomputed through the wiring below
//! and must equal the recorded one, and amendment 7's verdict is computed
//! through the same wiring.
//!
//! **Wiring** (the verdict path `renorm` ran, with the recorded conventions
//! in place of `FormPost::conventions`'s draw and each code's form step;
//! `ITEM_RULE`): an item with one request takes `FormPost::single`'s step (a
//! mirror or span through `child_v2`, a spliced edit through `item_v2`, then
//! the window on the final document); an item with several requests is
//! judged in two bindings, as `renorm::judge` judges it (each request with
//! its siblings pending, then the last with its siblings accepted when each
//! earlier one passed). A local child's verdict is
//! `renorm::reverdict_local`'s on that step, a spliced child's
//! `renorm::reverdict_spliced`'s, a rows row's `engine::simple_verdict`.
use crate::generation::{self, PostOut, PostProcess};
use crate::plan::Request;
use crate::renorm::{self, Batch, LineSplit, line_split};
use crate::splice;
use crate::text;
use anyhow::{Context, Result, bail, ensure};
use grammar_a_dryrun::engine::{self, Code, Edit, Failure, Slot, Source};
use grammar_a_dryrun::tally::{Case, Fin, Recorded, Tally};
use grammar_a_form::profile::replacement_lead;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use tokenizers::Tokenizer;

/// The run population of every batch the dry run re-verdicted (R0's Train
/// positives join `train2`, amendment 10 item 10.8).
pub const POPULATION: &str = "train2";

/// The one split the counts parse (BUILD-PLAN 12.3: "Train lines only").
pub const TRAIN: &str = "train";

fn sha(b: &[u8]) -> String {
    text::sha_hex(b)
}

fn file_sha(p: &Path) -> Result<String> {
    Ok(sha(&fs::read(p).with_context(|| p.display().to_string())?))
}

fn strs(v: &Value) -> Vec<String> {
    v.as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default()
}

fn sorted(mut v: Vec<String>) -> Vec<String> {
    v.sort();
    v.dedup();
    v
}

/// A summary file, refused unless its SHA-256 is `sha256`.
pub fn checked_summary(p: &Path, sha256: &str) -> Result<Value> {
    let b = fs::read(p).with_context(|| p.display().to_string())?;
    ensure!(sha(&b) == sha256, "{}: SHA-256 {} is not the recorded {sha256}", p.display(), sha(&b));
    Ok(serde_json::from_slice(&b)?)
}

/// `p`, refused unless its SHA-256 is `want` (a summary's recorded digest).
fn checked(p: &Path, want: &Value) -> Result<Value> {
    let h = file_sha(p)?;
    ensure!(want.as_str() == Some(h.as_str()), "{}: SHA-256 {h} is not the recorded {want}", p.display());
    Ok(json!({"path": p, "sha256": h}))
}

/// Streams the lines of JSONL file `p` whose split (`key`, read from the
/// line's bytes) `keep` takes, parsed; other lines are skipped unparsed and
/// counted by split in `skipped`. A line whose split cannot be read from
/// its bytes refuses the file, by line number only.
fn split_lines(p: &Path, key: &str, keep: &dyn Fn(&str) -> bool, skipped: &mut BTreeMap<String, usize>, mut f: impl FnMut(Value) -> Result<()>) -> Result<()> {
    let r = BufReader::new(fs::File::open(p).with_context(|| p.display().to_string())?);
    for (n, l) in r.split(b'\n').enumerate() {
        let l = l?;
        if l.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        match line_split(&l, key) {
            LineSplit::One(s) if keep(&s) => f(serde_json::from_slice(&l).with_context(|| format!("{} line {}", p.display(), n + 1))?)?,
            LineSplit::One(s) => *skipped.entry(s).or_default() += 1,
            _ => bail!("{} line {}: no {key} that can be read from the line's bytes; refused before any parse", p.display(), n + 1),
        }
    }
    Ok(())
}

/// R0's Train build rows by id: (source kind, origin, domain).
pub type R0Rows = HashMap<String, (String, String, String)>;

/// `p`'s bytes, refused unless their SHA-256 is `sha256` (the digest bound
/// with the dry run), before any line is read.
fn bound(p: &Path, sha256: &str) -> Result<String> {
    let h = file_sha(p)?;
    ensure!(h == sha256, "{}: SHA-256 {h} is not the bound {sha256}; refused before any line is read", p.display());
    Ok(h)
}

/// R0's Train build rows (refused unless their SHA-256 is `sha256`): id ->
/// (source kind, origin, domain). Lines of another split are skipped
/// unparsed and counted.
pub fn r0_rows(p: &Path, sha256: &str) -> Result<(R0Rows, Value)> {
    let h = bound(p, sha256)?;
    let mut out = HashMap::new();
    let mut skipped = BTreeMap::new();
    split_lines(p, "split", &|s| s == "train", &mut skipped, |v| {
        let g = |k: &str| v[k].as_str().map(String::from).with_context(|| format!("R0 row {k}"));
        ensure!(out.insert(g("id")?, (g("source_kind")?, g("origin")?, g("domain")?)).is_none(), "duplicate R0 row id");
        Ok(())
    })?;
    let rec = json!({"path": p, "sha256": h, "rows": out.len(), "lines_of_other_splits_skipped_unparsed": skipped});
    Ok((out, rec))
}

/// The S0+ snapshot's Train item ids (refused unless its SHA-256 is
/// `sha256`).
pub fn snapshot_ids(p: &Path, sha256: &str) -> Result<(BTreeSet<String>, Value)> {
    let h = bound(p, sha256)?;
    let mut out = BTreeSet::new();
    let mut skipped = BTreeMap::new();
    split_lines(p, "split", &|s| s == "train", &mut skipped, |v| {
        out.insert(v["id"].as_str().context("snapshot id")?.to_string());
        Ok(())
    })?;
    let rec = json!({"path": p, "sha256": h, "items": out.len(), "lines_of_other_splits_skipped_unparsed": skipped});
    Ok((out, rec))
}

/// A re-verdicted record response of an output journal: what the count
/// reads of it.
#[derive(Clone, Debug)]
struct Journaled {
    accepted: bool,
    reasons: Vec<String>,
    /// The laid-out child and the raw child.
    text: String,
    raw: String,
    /// The profile record's conventions.
    conv: Value,
    /// The verdict's `finish_reason` and `raw_words` (`reverdict_local`
    /// copies them into its diag).
    orig: Value,
    /// The response's verdict reasons before the re-verdict (its `renorm`
    /// record's `original_reasons`).
    original_reasons: Vec<String>,
}

/// The re-verdicted record responses by (request id, attempt).
type JournalVerdicts = HashMap<(String, u32), Journaled>;

/// The re-verdicted record responses of a batch's output journals, by
/// (request id, attempt): Train lines only, each line's split read from its
/// bytes before any parse (a line naming no split is parsed only when its
/// `request_id`, read from its bytes, is the batch's), with a `renorm`
/// record that is not a superseded response's. Every other line is skipped
/// unparsed and counted. Each journal file is checked against the summary's
/// digest.
fn journal_verdicts(out: &Path, summary: &Value, batch: &Batch) -> Result<(JournalVerdicts, Value)> {
    let mut found = HashMap::new();
    let mut files = Vec::new();
    let mut skipped = 0usize;
    for j in summary["outputs"]["journals"].as_array().context("summary outputs.journals")? {
        let p = out.join(j["path"].as_str().context("journal path")?);
        files.push(checked(&p, &j["sha256"])?);
        let r = BufReader::new(fs::File::open(&p)?);
        for l in r.split(b'\n') {
            let l = l?;
            if l.iter().all(u8::is_ascii_whitespace) {
                continue;
            }
            // Train lines only (BUILD-PLAN 12.3): a train line, or a line naming no split whose request_id
            // bytes name one of the batch's requests (an item check's record names its siblings' request
            // ids, so a re-verdicted line's request_id bytes can name several; its split is train)
            let split = line_split(&l, "split");
            let ours = matches!(line_split(&l, "request_id"), LineSplit::One(r) if batch.requests.contains_key(&r));
            let train = split == LineSplit::One(TRAIN.to_string());
            if !(train || (split == LineSplit::Missing && ours)) {
                skipped += 1;
                continue;
            }
            let v: Value = serde_json::from_slice(&l).with_context(|| p.display().to_string())?;
            let rid = v["request_id"].as_str().context("request_id")?.to_string();
            if !batch.requests.contains_key(&rid) {
                skipped += 1;
                continue;
            }
            if v["kind"] != "response" || !v["renorm"].is_object() || !v["renorm"]["rule"].is_null() {
                continue;
            }
            let a = v["attempt"].as_u64().context("attempt")? as u32;
            let vd = &v["verdict"];
            let j = Journaled {
                accepted: vd["accepted"] == true,
                reasons: strs(&vd["reasons"]),
                text: vd["text"].as_str().with_context(|| format!("request {rid}: verdict text"))?.to_string(),
                raw: vd["raw_text"].as_str().with_context(|| format!("request {rid}: verdict raw_text"))?.to_string(),
                conv: vd["diag"]["post"]["conventions"].clone(),
                orig: json!({"diag": {"finish_reason": vd["diag"]["finish_reason"], "raw_words": vd["diag"]["raw_words"]}}),
                original_reasons: strs(&v["renorm"]["original_reasons"]),
            };
            ensure!(found.insert((rid.clone(), a), j).is_none(), "request {rid} attempt {a}: two re-verdicted responses");
        }
    }
    let rec = json!({"files": files, "reverdicted_responses": found.len(), "lines_skipped_unparsed": skipped});
    Ok((found, rec))
}

// ------------------------------------------------------------------ wiring

/// A precomputed profile step as a `PostProcess` (the landed verdict
/// functions read it).
struct Fixed {
    text: String,
    reasons: Vec<String>,
}

impl PostProcess for Fixed {
    fn apply(&self, _r: &Request, _parent: Option<&str>, _clean: &str) -> PostOut {
        PostOut { text: self.text.clone(), reasons: self.reasons.clone(), record: json!({}) }
    }
    fn is_profile_v2(&self) -> bool {
        true
    }
}

/// `FormPost`'s window on a final document, under `code`.
fn window(code: Code, doc: Result<String, String>, reasons: &mut Vec<String>) {
    match doc {
        Ok(d) => reasons.extend(engine::window_reasons(code, engine::words(code, &d))),
        Err(e) => reasons.push(format!("window_assembly_error:{e}")),
    }
}

/// One request's splice in an item (`FormPost`'s `Part`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Part {
    pub request_id: String,
    pub span: (usize, usize),
    pub insert_sep: Option<String>,
}

/// The final document of an item, as `FormPost::assemble` (and finalize)
/// assembles it: `splice::assemble_parts` for a spliced plan, else
/// `generation::assemble`. Returns the text and each part's final span.
fn assemble(spliced: bool, parent: &str, parts: &[&Part], reps: &[&str]) -> Result<(String, Vec<(usize, usize)>), String> {
    if spliced {
        let ps: Vec<splice::Part> = parts.iter().map(|p| splice::Part { parent_span: p.span, insert_sep: p.insert_sep.as_deref() }).collect();
        splice::assemble_parts(parent, &ps, reps).map(|a| (a.text, a.edit_spans)).map_err(|e| e.to_string())
    } else {
        let spans: Vec<(usize, usize)> = parts.iter().map(|p| p.span).collect();
        generation::assemble(parent, &spans, reps).map(|x| (x.0, x.1)).map_err(|e| e.to_string())
    }
}

/// `FormPost::single`'s step under `code` with the recorded conventions:
/// (laid-out text, reasons).
fn single_step(code: Code, spliced: Option<&Part>, r: &Request, p: &str, conv: &Value, clean: &str) -> Result<(String, Vec<String>), Failure> {
    let (text, mut reasons, doc) = match spliced {
        Some(e) => {
            let it = engine::item_v2(code, p, conv, &[Edit { span: e.span, insert_sep: e.insert_sep.as_deref(), raw: clean }])?;
            let text = it.reps.into_iter().next().unwrap_or_default();
            let doc = (!text.is_empty()).then(|| assemble(true, p, &[e], &[text.as_str()]).map(|x| x.0));
            (text, it.reasons, doc)
        }
        None => {
            let ch = engine::child_v2(code, p, r.source_span, conv, clean)?;
            let doc = (!ch.text.is_empty()).then(|| match r.source_span {
                None => Ok(ch.text.clone()),
                Some(sp) => generation::assemble(p, &[sp], &[ch.text.as_str()]).map(|x| x.0).map_err(|e| e.to_string()),
            });
            (ch.text, ch.reasons, doc)
        }
    };
    if let Some(d) = doc {
        window(code, d, &mut reasons);
    }
    Ok((text, sorted(reasons)))
}

/// `FormPost::of_item`'s step for request `idx` of an item under `code`.
/// `siblings` is `None` while the item's other requests are pending (the
/// own-edit rules; the deferred paragraph reasons reject nothing; the edit
/// laid out in its item with the raw texts `context` gives, in item order,
/// `idx`'s own entry unused: `engine::edit_in_item_v2`), else each
/// sibling's (raw child, accepted laid-out text) in item order (`idx`'s own
/// entry is unused): the item check.
#[allow(clippy::too_many_arguments)]
fn item_step(
    code: Code,
    spliced: bool,
    parts: &[Part],
    idx: usize,
    p: &str,
    conv: &Value,
    clean: &str,
    siblings: Option<&[(String, String)]>,
    context: &[Option<&str>],
) -> Result<(String, Vec<String>), Failure> {
    let Some(sib) = siblings else {
        let slots: Vec<Slot> = parts
            .iter()
            .enumerate()
            .map(|(j, part)| Slot { span: part.span, insert_sep: part.insert_sep.as_deref(), raw: if j == idx { Some(clean) } else { context.get(j).copied().flatten() } })
            .collect();
        let it = engine::edit_in_item_v2(code, p, conv, &slots, idx)?;
        let mut mine = BTreeSet::new();
        for e in it.record["edits"].as_array().into_iter().flatten() {
            mine.extend(strs(&e["form_reasons"]));
        }
        let deferred: BTreeSet<String> = strs(&it.record["form_reasons"]).into_iter().filter(|x| !mine.contains(x)).collect();
        let reasons: Vec<String> = it.reasons.iter().filter(|x| !deferred.contains(*x)).cloned().collect();
        return Ok((it.reps.into_iter().next().unwrap_or_default(), sorted(reasons)));
    };
    let raws: Vec<&str> = (0..parts.len()).map(|j| if j == idx { clean } else { sib[j].0.as_str() }).collect();
    let edits: Vec<Edit> = parts.iter().zip(&raws).map(|(part, raw)| Edit { span: part.span, insert_sep: part.insert_sep.as_deref(), raw }).collect();
    let it = engine::item_v2(code, p, conv, &edits)?;
    let mut reasons = it.reasons.clone();
    for j in (0..parts.len()).filter(|&j| j != idx) {
        if it.reps.get(j) != Some(&sib[j].1) {
            reasons.push("item_sibling_text_mismatch".into());
        }
    }
    let text = it.reps.get(idx).cloned().unwrap_or_default();
    if it.reps.iter().all(|t| !t.is_empty()) {
        let ps: Vec<&Part> = parts.iter().collect();
        let reps: Vec<&str> = it.reps.iter().map(String::as_str).collect();
        let doc = assemble(spliced, p, &ps, &reps).map(|x| x.0);
        if let (Ok(d), Some(a)) = (&doc, &it.assembled)
            && *d != a.0
        {
            reasons.push("item_assembly_mismatch".into());
        }
        window(code, doc, &mut reasons);
    }
    Ok((text, sorted(reasons)))
}

/// One child of a local or spliced batch: its request, its recorded
/// verdict and what the verdict path read.
#[derive(Clone, Debug)]
struct Child {
    raw: String,
    /// The recorded laid-out child.
    text: String,
    conv: Value,
    /// `orig` for `reverdict_local`'s diag fields.
    orig: Value,
    /// The spliced verdict's original reasons.
    original_reasons: Vec<String>,
}

/// One child's recorded outcome, from the re-verdict records.
struct Rec {
    item_id: String,
    request_id: String,
    domain: String,
    accepted: bool,
    reasons: Vec<String>,
    text_sha256: String,
}

/// One code's verdicts of every child of a batch, as `renorm::judge`
/// judges them (`ITEM_RULE`): (accepted, reasons, laid-out text).
type Verdicts = HashMap<String, CodeVerdict>;

/// One child's verdict under one code: (accepted, reasons, laid-out text).
type CodeVerdict = Result<(bool, Vec<String>, String), Failure>;

fn judge_code(code: Code, b: &Wired, children: &HashMap<String, Child>) -> Result<Verdicts> {
    let verdict_of = |r: &Request, parent: &str, ch: &Child, step: (String, Vec<String>)| -> Result<(bool, Vec<String>, String)> {
        let post = Fixed { text: step.0, reasons: step.1 };
        let v = match b.batch.kind {
            renorm::PlanKind::Local => renorm::reverdict_local(&post, r, Some(parent), &ch.raw, &ch.orig, b.tokenizer.context("a local batch needs its tokenizer")?),
            renorm::PlanKind::Spliced => renorm::reverdict_spliced(&post, r, parent, &ch.raw, &json!({"reasons": ch.original_reasons, "diag": {}})),
        };
        Ok((v["accepted"] == true, strs(&v["reasons"]), v["text"].as_str().unwrap_or_default().to_string()))
    };
    let mut out: Verdicts = HashMap::new();
    for it in &b.batch.items {
        let parent = b.batch.parents.get(&it.parent_id).with_context(|| format!("item {}: parent missing", it.item_id))?.as_str();
        match &it.spec {
            None => {
                for rid in &it.requests {
                    let Some(ch) = children.get(rid) else { continue };
                    let r = &b.batch.requests[rid];
                    let res = single_step(code, b.edits.get(rid), r, parent, &ch.conv, &ch.raw);
                    out.insert(rid.clone(), match res {
                        Ok(step) => Ok(verdict_of(r, parent, ch, step)?),
                        Err(f) => Err(f),
                    });
                }
            }
            Some(spec) => {
                let parts: Vec<Part> = spec.parts.iter().map(|(rid, span, sep)| Part { request_id: rid.clone(), span: *span, insert_sep: sep.clone() }).collect();
                // (a) each request with its siblings pending, laid out in its
                // item with the raw texts of the requests before it that
                // passed (renorm's layout context; amendment 29 item 29.1)
                let mut first: HashMap<&str, CodeVerdict> = HashMap::new();
                let mut context: Vec<Option<&str>> = vec![None; parts.len()];
                for (idx, part) in parts.iter().enumerate() {
                    let Some(ch) = children.get(&part.request_id) else { continue };
                    let r = &b.batch.requests[&part.request_id];
                    let res = item_step(code, b.spliced(), &parts, idx, parent, &ch.conv, &ch.raw, None, &context);
                    let v = match res {
                        Ok(step) => Ok(verdict_of(r, parent, ch, step)?),
                        Err(f) => Err(f),
                    };
                    if matches!(v, Ok((true, _, _))) {
                        context[idx] = Some(ch.raw.as_str());
                    }
                    first.insert(part.request_id.as_str(), v);
                }
                // (b) the last with every sibling accepted, when each earlier one passed (a)
                let complete = parts.iter().all(|p| children.contains_key(&p.request_id));
                let n = parts.len();
                let head_ok = parts[..n - 1].iter().all(|p| matches!(first.get(p.request_id.as_str()), Some(Ok((true, _, _)))));
                let mut last = None;
                if complete && head_ok {
                    let sib: Vec<(String, String)> = parts
                        .iter()
                        .map(|p| (children[&p.request_id].raw.clone(), first.get(p.request_id.as_str()).and_then(|x| x.as_ref().ok()).map(|x| x.2.clone()).unwrap_or_default()))
                        .collect();
                    let lp = &parts[n - 1];
                    let ch = &children[&lp.request_id];
                    let r = &b.batch.requests[&lp.request_id];
                    last = Some(match item_step(code, b.spliced(), &parts, n - 1, parent, &ch.conv, &ch.raw, Some(&sib), &[]) {
                        Ok(step) => Ok(verdict_of(r, parent, ch, step)?),
                        Err(f) => Err(f),
                    });
                }
                for (rid, v) in first {
                    out.insert(rid.to_string(), v);
                }
                if let Some(v) = last {
                    out.insert(parts[n - 1].request_id.clone(), v);
                }
            }
        }
    }
    Ok(out)
}

/// A batch with what its wiring needs.
struct Wired<'a> {
    batch: Batch<'a>,
    /// Spliced plans: each request's edit.
    edits: HashMap<String, Part>,
    tokenizer: Option<&'a Tokenizer>,
}

impl Wired<'_> {
    fn spliced(&self) -> bool {
        self.batch.kind == renorm::PlanKind::Spliced
    }
}

/// One counted batch: its tally and the inputs it read, with their digests.
pub struct Counted {
    pub tally: Tally,
    pub inputs: Value,
    pub children: Value,
}

/// Counts a local or spliced batch's children (see the module notes).
/// `r0` gives a local child's class and domain; `snapshot` restricts a
/// spliced batch to the S0+ snapshot's items.
fn count_journal_batch(label: &str, out: &Path, summary: &Value, b: &Wired, r0: Option<&R0Rows>, snapshot: Option<&BTreeSet<String>>) -> Result<Counted> {
    let (journals, jrec) = journal_verdicts(out, summary, &b.batch)?;
    let rv = &summary["outputs"]["reverdict"];
    let rv_path = out.join(rv["path"].as_str().context("summary outputs.reverdict.path")?);
    let rv_rec = checked(&rv_path, &rv["sha256"])?;
    // the children: requests of complete R0 items whose record response the journals hold
    let mut children: HashMap<String, Child> = HashMap::new();
    let mut cases: Vec<Rec> = Vec::new();
    let mut seen = BTreeMap::<&str, usize>::new();
    for l in BufReader::new(fs::File::open(&rv_path)?).lines() {
        let l = l?;
        if l.trim().is_empty() {
            continue;
        }
        let it: Value = serde_json::from_str(&l)?;
        let r0_item = match b.batch.kind {
            renorm::PlanKind::Local => it["r0_record"] == true,
            renorm::PlanKind::Spliced => snapshot.is_some_and(|s| s.contains(it["item_id"].as_str().unwrap_or_default())),
        };
        if !r0_item {
            *seen.entry("items_not_r0_records").or_default() += 1;
            continue;
        }
        *seen.entry("r0_items").or_default() += 1;
        if it["complete_in_journals"] != true {
            *seen.entry("r0_items_not_complete").or_default() += 1;
            continue;
        }
        for q in it["requests"].as_array().context("requests")? {
            if q["accepted_in_journals"] != true {
                continue;
            }
            let rid = q["request_id"].as_str().context("request_id")?.to_string();
            let a = q["attempt"].as_u64().context("attempt")? as u32;
            let j = journals.get(&(rid.clone(), a)).with_context(|| format!("request {rid} attempt {a}: no re-verdicted response in the output journals"))?;
            let text_sha256 = q["text_sha256"].as_str().unwrap_or_default().to_string();
            ensure!(sha(j.text.as_bytes()) == text_sha256, "request {rid}: the journal's laid-out text is not the record's");
            ensure!(sha(j.raw.as_bytes()) == q["raw_response_sha256"].as_str().unwrap_or_default(), "request {rid}: the journal's raw text is not the record's");
            ensure!(j.accepted == (q["accepted"] == true) && j.reasons == strs(&q["reasons"]), "request {rid}: the journal's verdict is not the record's");
            ensure!(j.conv.is_object(), "request {rid}: the profile record holds no conventions");
            children.insert(rid.clone(), Child { raw: j.raw.clone(), text: j.text.clone(), conv: j.conv.clone(), orig: j.orig.clone(), original_reasons: j.original_reasons.clone() });
            cases.push(Rec {
                item_id: it["item_id"].as_str().context("item_id")?.to_string(),
                request_id: rid,
                domain: it["domain"].as_str().unwrap_or_default().to_string(),
                accepted: j.accepted,
                reasons: j.reasons.clone(),
                text_sha256,
            });
        }
    }
    drop(journals);
    let landed = judge_code(Code::Landed, b, &children)?;
    let a7 = judge_code(Code::A7, b, &children)?;
    let source = match b.batch.kind {
        renorm::PlanKind::Local => "local",
        renorm::PlanKind::Spliced => "spliced",
    };
    let mut tally = Tally::default();
    let items: HashMap<&str, &renorm::RItem> = b.batch.items.iter().map(|i| (i.item_id.as_str(), i)).collect();
    for q in &cases {
        let rid = q.request_id.as_str();
        let item_id = q.item_id.as_str();
        let ri = items.get(item_id).with_context(|| format!("item {item_id} is not the batch's"))?;
        let (class, domain) = match b.batch.kind {
            renorm::PlanKind::Local => {
                let (sk, origin, dom) = r0.and_then(|m| m.get(item_id)).with_context(|| format!("local item {item_id} is no R0 Train build row"))?;
                ensure!(sk == "local_positive", "item {item_id}: R0 row source kind {sk}");
                (origin.clone(), dom.clone())
            }
            renorm::PlanKind::Spliced => ("mixed".to_string(), q.domain.clone()),
        };
        let ch = &children[rid];
        let r = &b.batch.requests[rid];
        let parent = b.batch.parents[&ri.parent_id].as_str();
        let part = match (&ri.spec, b.edits.get(rid)) {
            (_, Some(e)) => e.clone(),
            (Some(spec), None) => spec.parts.iter().find(|x| x.0 == rid).map(|(_, s, sep)| Part { request_id: rid.to_string(), span: *s, insert_sep: sep.clone() }).context("item part")?,
            (None, None) => Part { request_id: rid.to_string(), span: r.source_span.unwrap_or((0, 0)), insert_sep: None },
        };
        let item_parts: Vec<Part> = match &ri.spec {
            Some(spec) => spec.parts.iter().map(|(rid, s, sep)| Part { request_id: rid.clone(), span: *s, insert_sep: sep.clone() }).collect(),
            None => vec![part.clone()],
        };
        let k = item_parts.iter().position(|p| p.request_id == rid).unwrap_or(0);
        // an item's edit is laid out in its item with the raw texts the
        // verdict path's layout read: its own, and those of the requests
        // before it (item order) that passed (amendment 29 item 29.1)
        let passed = |p: &Part| matches!(landed.get(&p.request_id), Some(Ok((true, _, _))));
        let slots: Vec<Slot> = item_parts
            .iter()
            .enumerate()
            .map(|(j, p)| Slot {
                span: p.span,
                insert_sep: p.insert_sep.as_deref(),
                raw: (j == k || (j < k && passed(p))).then(|| children.get(&p.request_id).map(|c| c.raw.as_str())).flatten(),
            })
            .collect();
        let src = if ri.spec.is_some() {
            Source::InItem { parent, slots: &slots, index: k }
        } else if b.spliced() {
            if part.span.0 == part.span.1 { Source::Insertion { parent, at: part.span.0, sep: part.insert_sep.as_deref().unwrap_or("") } } else { Source::Span { parent, span: part.span } }
        } else {
            match r.source_span {
                None => Source::Mirror { parent },
                Some(span) => Source::Span { parent, span },
            }
        };
        // the final document: the parent with every request's recorded text
        // of the item; the child's range there is its replacement's, a
        // leading line break and lead included (amendment 29 item 29.1)
        let mut doc: Option<(String, (usize, usize))> = None;
        if !src.whole() && item_parts.iter().all(|p| children.contains_key(&p.request_id)) {
            let reps: Vec<&str> = item_parts.iter().map(|p| children[&p.request_id].text.as_str()).collect();
            let ps: Vec<&Part> = item_parts.iter().collect();
            if let Ok((d, spans)) = assemble(b.spliced(), parent, &ps, &reps) {
                let lead = replacement_lead(reps[k]).unwrap_or(0);
                doc = Some((d, (spans[k].0 - lead, spans[k].1)));
            }
        }
        let lv = landed.get(rid).cloned().unwrap_or(Err(Failure::Unsupported)).map(|x| (x.0, x.1));
        let av = a7.get(rid).cloned().unwrap_or(Err(Failure::Unsupported)).map(|x| (x.0, x.1));
        let fin = match &doc {
            Some((d, r)) => Fin::Doc(d, *r),
            None if src.whole() => Fin::Whole,
            None => Fin::Unassembled,
        };
        tally.count(&Case {
            batch: label,
            source,
            class: &class,
            domain: &domain,
            item_id,
            request_id: Some(rid),
            src,
            raw: &ch.raw,
            conv: &ch.conv,
            recorded: Recorded { accepted: q.accepted, reasons: &q.reasons, text_sha256: &q.text_sha256 },
            landed: lv,
            a7: av,
            fin,
        });
    }
    let children_rec = json!({"children": cases.len(), "items": seen, "journals": jrec});
    let inputs = json!({"reverdict": rv_rec, "journals": jrec["files"]});
    Ok(Counted { tally, inputs, children: children_rec })
}

/// Counts a local batch: `out` its renorm output directory, `summary_sha256`
/// its summary's recorded digest, `r0_rows` R0's Train build rows.
pub fn count_local(label: &str, out: &Path, summary_sha256: &str, r0_rows_path: &Path, r0_rows_sha256: &str) -> Result<Counted> {
    let sp = out.join("RENORM-split-train.json");
    let s = checked_summary(&sp, summary_sha256)?;
    ensure!(s["schema"] == renorm::RENORM_SCHEMA && s["plan_kind"] == "local" && s["split"] == "train", "{}: not a local Train batch summary", sp.display());
    let pdir = PathBuf::from(s["inputs"]["plan"].as_str().context("inputs.plan")?);
    let tok_path = PathBuf::from(s["inputs"]["tokenizer"]["path"].as_str().context("inputs.tokenizer")?);
    let tok_rec = checked(&tok_path, &s["inputs"]["tokenizer"]["sha256"])?;
    let mut tok = Tokenizer::from_file(&tok_path).map_err(|e| anyhow::anyhow!(e.to_string()))?;
    tok.with_truncation(None).map_err(|e| anyhow::anyhow!(e.to_string()))?;
    tok.with_padding(None);
    let population = s["profile"]["population"].as_str().context("profile.population")?;
    ensure!(population == POPULATION, "population {population}");
    let batch = renorm::local_batch(&pdir, "train", population, &tok)?;
    check_batch_inputs(&batch, &s)?;
    let (r0, r0_rec) = r0_rows(r0_rows_path, r0_rows_sha256)?;
    let w = Wired { batch, edits: HashMap::new(), tokenizer: Some(&tok) };
    let mut c = count_journal_batch(label, out, &s, &w, Some(&r0), None)?;
    c.inputs["summary"] = json!({"path": sp, "sha256": summary_sha256});
    c.inputs["tokenizer"] = tok_rec;
    c.inputs["r0_rows"] = r0_rec;
    c.inputs["plan"] = w.batch.inputs.clone();
    c.inputs["plan_lines_skipped_unparsed"] = json!(w.batch.skipped);
    Ok(c)
}

/// The batch rebuilt from its plan must be the summary's: the same plan,
/// items, request files and input digests, and the same prior.
fn check_batch_inputs(batch: &Batch, s: &Value) -> Result<()> {
    for k in ["plan_sha256", "items_sha256", "request_files", "input", "parents_sha256", "plan_population"] {
        if !s["inputs"][k].is_null() || !batch.inputs[k].is_null() {
            ensure!(batch.inputs[k] == s["inputs"][k], "the plan's {k} is not the summary's");
        }
    }
    ensure!(batch.form.prior_sha256() == s["prior_sha256"].as_str().unwrap_or_default(), "the rebuilt prior {} is not the summary's {}", batch.form.prior_sha256(), s["prior_sha256"]);
    Ok(())
}

/// Counts the spliced batch: `out` its renorm output directory,
/// `summary_sha256` its summary's recorded digest, `snapshot` the S0+
/// snapshot's Train records (R0's spliced items) and `snapshot_sha256` their
/// bound digest. The plan's items, parents and request files are read Train
/// lines only (`renorm::spliced_batch_of`), each line's split from its bytes
/// first; every other line is skipped unparsed and counted.
pub fn count_spliced(label: &str, out: &Path, summary_sha256: &str, snapshot: &Path, snapshot_sha256: &str) -> Result<Counted> {
    let sp = out.join("RENORM-population-train.json");
    let s = checked_summary(&sp, summary_sha256)?;
    ensure!(s["schema"] == renorm::RENORM_SCHEMA && s["plan_kind"] == "spliced" && s["split"] == "train", "{}: not a spliced Train batch summary", sp.display());
    let pdir = PathBuf::from(s["inputs"]["plan"].as_str().context("inputs.plan")?);
    let plan_population = s["inputs"]["plan_population"].as_str().context("inputs.plan_population")?;
    let population = s["profile"]["population"].as_str().context("profile.population")?;
    ensure!(population == POPULATION, "population {population}");
    let train = |x: &str| x == TRAIN;
    let batch = renorm::spliced_batch_of(&pdir, plan_population, population, &train)?;
    ensure!(batch.split == TRAIN, "plan population {plan_population} is of split {}, not train", batch.split);
    check_batch_inputs(&batch, &s)?;
    // each request's edit, from the plan's items (split read from each line's bytes first)
    let mut edits: HashMap<String, Part> = HashMap::new();
    let mut skipped = BTreeMap::new();
    split_lines(&pdir.join("items.jsonl"), "split", &train, &mut skipped, |v| {
        let it: splice::Item = serde_json::from_value(v)?;
        if it.population == plan_population {
            for e in it.edits {
                edits.insert(e.request_id.clone(), Part { request_id: e.request_id, span: e.parent_span, insert_sep: e.insert_sep });
            }
        }
        Ok(())
    })?;
    ensure!(batch.requests.keys().all(|r| edits.contains_key(r)), "a request of the batch has no edit in the plan's items");
    let (snap, snap_rec) = snapshot_ids(snapshot, snapshot_sha256)?;
    let w = Wired { batch, edits, tokenizer: None };
    let mut c = count_journal_batch(label, out, &s, &w, None, Some(&snap))?;
    c.inputs["summary"] = json!({"path": sp, "sha256": summary_sha256});
    c.inputs["snapshot"] = snap_rec;
    c.inputs["plan"] = w.batch.inputs.clone();
    c.inputs["plan_items_lines_skipped_unparsed"] = json!(skipped);
    c.inputs["plan_lines_skipped_unparsed"] = json!(w.batch.skipped);
    Ok(c)
}

/// Counts the rows batch (RAID, stage-3 legacy and hosted-capture rows):
/// `out` its output directory, `summary_sha256` its summary's recorded
/// digest. Every planned row with a transfer record is a child; its parent
/// or donor is the summary's rows file row or RAID document its
/// `layout_source` names, its raw child the row's text (a span row's one
/// span).
pub fn count_rows(label: &str, out: &Path, summary_sha256: &str) -> Result<Counted> {
    let sp = out.join("ROWS-RENORM.json");
    let s = checked_summary(&sp, summary_sha256)?;
    ensure!(s["schema"] == renorm::ROWS_RENORM_SCHEMA, "{}: not a rows summary", sp.display());
    ensure!(s["population"] == POPULATION, "population {}", s["population"]);
    let rows_path = PathBuf::from(s["inputs"]["rows"]["path"].as_str().context("inputs.rows.path")?);
    let rows_rec = checked(&rows_path, &s["inputs"]["rows"]["sha256"])?;
    ensure!(s["inputs"]["donors"]["same_as_rows"] == true, "the rows summary's donors are another file");
    let mut skipped = BTreeMap::new();
    let mut rows: HashMap<String, renorm::Row> = HashMap::new();
    split_lines(&rows_path, "split", &|x| x == "train", &mut skipped, |v| {
        let r = renorm::Row::from_value(&v)?;
        ensure!(rows.insert(r.id.clone(), r).is_none(), "duplicate row id");
        Ok(())
    })?;
    let mut raid: HashMap<String, renorm::RaidHuman> = HashMap::new();
    let mut raid_rec = Value::Null;
    if let Some(p) = s["inputs"]["raid_documents"]["path"].as_str() {
        let p = PathBuf::from(p);
        raid_rec = checked(&p, &s["inputs"]["raid_documents"]["sha256"])?;
        let groups: BTreeSet<&str> = rows
            .values()
            .filter(|r| r.source_kind == "external_pool" && r.cohort_id.starts_with(renorm::RAID_COHORT_PREFIX) && r.origin == "model_only")
            .map(|r| r.source_group.as_str())
            .collect();
        for h in renorm::raid_humans(&p, &groups)? {
            raid.insert(h.id.clone(), h);
        }
    }
    let rv = &s["outputs"]["reverdict"];
    let rv_path = out.join(rv["path"].as_str().context("outputs.reverdict.path")?);
    let rv_rec = checked(&rv_path, &rv["sha256"])?;
    let mut tally = Tally::default();
    let mut seen: BTreeMap<String, usize> = BTreeMap::new();
    for l in BufReader::new(fs::File::open(&rv_path)?).lines() {
        let l = l?;
        if l.trim().is_empty() {
            continue;
        }
        let v: Value = serde_json::from_str(&l)?;
        *seen.entry("planned_rows".into()).or_default() += 1;
        let rec = &v["record"];
        if rec["transfer"].is_null() {
            *seen.entry("rows_without_transfer_record".into()).or_default() += 1;
            continue;
        }
        let id = v["id"].as_str().context("row id")?;
        let row = rows.get(id).with_context(|| format!("row {id} is not a Train row of the rows file"))?;
        let ls = &v["layout_source"];
        let lid = ls["id"].as_str().context("layout_source.id")?;
        let text_of = |i: &str| text_in(&rows, &raid, i).with_context(|| format!("row {id}: layout source {i} is no row or RAID document"));
        let parent_span = match ls["parent_span"].as_array() {
            Some(a) => Some((a[0].as_u64().context("parent_span")? as usize, a[1].as_u64().context("parent_span")? as usize)),
            None => None,
        };
        let (src, raw) = match (ls["kind"].as_str(), parent_span) {
            (Some("layout_donor"), _) => (Source::Donor { donor: text_of(lid)? }, row.text.as_str()),
            (_, Some(span)) => {
                let [rs] = row.ai_byte_spans.as_slice() else { bail!("row {id}: a span row with {} spans", row.ai_byte_spans.len()) };
                (Source::Span { parent: text_of(lid)?, span }, row.text.get(rs.0..rs.1).context("row span")?)
            }
            (_, None) => (Source::Mirror { parent: text_of(lid)? }, row.text.as_str()),
        };
        ensure!(rec["raw_response_sha256"].as_str() == Some(sha(raw.as_bytes()).as_str()), "row {id}: the raw child is not the record's");
        let conv = &rec["conventions"];
        let reasons = strs(&v["reasons"]);
        let lv = engine::simple_verdict(Code::Landed, src, conv, raw).map(|x| (x.accepted, x.reasons));
        let av = engine::simple_verdict(Code::A7, src, conv, raw).map(|x| (x.accepted, x.reasons));
        let text_sha = rec["output_sha256"].as_str().unwrap_or_default().to_string();
        let fin = match src {
            Source::Span { parent, span } => Fin::Splice { parent, span, doc_sha256: v["text_sha256"].as_str() },
            _ => Fin::Whole,
        };
        let (source, class, domain) = (v["source_kind"].as_str().unwrap_or_default(), v["origin"].as_str().unwrap_or_default(), v["domain"].as_str().unwrap_or_default());
        tally.count(&Case {
            batch: label,
            source,
            class,
            domain,
            item_id: id,
            request_id: None,
            src,
            raw,
            conv,
            recorded: Recorded { accepted: v["pass"] == true, reasons: &reasons, text_sha256: &text_sha },
            landed: lv,
            a7: av,
            fin,
        });
        *seen.entry("children".into()).or_default() += 1;
    }
    let inputs = json!({"summary": {"path": sp, "sha256": summary_sha256}, "rows": rows_rec, "raid_documents": raid_rec, "reverdict": rv_rec,
        "rows_lines_of_other_splits_skipped_unparsed": skipped});
    Ok(Counted { tally, inputs, children: json!(seen) })
}

/// A layout source's text: a row of the rows file, else a RAID document.
fn text_in<'m>(rows: &'m HashMap<String, renorm::Row>, raid: &'m HashMap<String, renorm::RaidHuman>, i: &str) -> Option<&'m str> {
    rows.get(i).map(|r| r.text.as_str()).or_else(|| raid.get(i).map(|h| h.text.as_str()))
}

// ------------------------------------------------------------------ records

/// Creates `path` with `bytes` (mode 600), refusing an existing file.
fn write_new(path: &Path, bytes: &[u8]) -> Result<String> {
    use std::io::Write;
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

fn jsonl(rows: &[Value]) -> Result<Vec<u8>> {
    let mut b = Vec::new();
    for r in rows {
        serde_json::to_writer(&mut b, r)?;
        b.push(b'\n');
    }
    Ok(b)
}

/// Writes one batch's record (`BATCH_SCHEMA`) at the new path `out` and
/// its ids-only file at the new path `ids_out` (mode 600): the ids behind
/// each non-zero stop invariant and each mismatch, Train records only, no
/// text. Returns the record; refuses (after writing both) when any child
/// mismatched.
pub fn write_batch(out: &Path, ids_out: &Path, label: &str, c: &Counted, code: &Value) -> Result<Value> {
    let ids = grammar_a_dryrun::ids_lines(&c.tally);
    let ih = write_new(ids_out, &jsonl(&ids)?)?;
    let mismatched = c.tally.mismatched();
    let rec = grammar_a_dryrun::batch_record(label, code, &c.inputs, &c.children, &c.tally, json!({"path": ids_out, "sha256": ih, "lines": ids.len()}));
    let mut b = serde_json::to_vec_pretty(&rec)?;
    b.push(b'\n');
    write_new(out, &b)?;
    ensure!(mismatched == 0, "{mismatched} children mismatch their records: the run stops ({} holds the counts by reason, {} the ids)", out.display(), ids_out.display());
    Ok(rec)
}

/// Merges batch records into `DRY-RUN-COUNTS.json` (`COUNTS_SCHEMA`) at the
/// new path `out`: refuses a batch that mismatched, a batch counted twice,
/// and any cell whose children or accepted children differ from
/// `DRY-RUN-FIGURES.json`'s (`figures`, refused unless its SHA-256 is
/// `figures_sha256`).
pub fn merge(batches: &[PathBuf], figures: &Path, figures_sha256: &str, out: &Path, code: &Value) -> Result<Value> {
    let fig = checked_summary(figures, figures_sha256)?;
    let mut recs = Vec::new();
    let mut inputs = Vec::new();
    let mut labels = BTreeSet::new();
    for p in batches {
        let b = fs::read(p).with_context(|| p.display().to_string())?;
        let v: Value = serde_json::from_slice(&b)?;
        ensure!(v["schema"] == grammar_a_dryrun::BATCH_SCHEMA, "{}: not a batch counts record", p.display());
        ensure!(v["counts"]["mismatched_children"] == 0 && v["stopped"].is_null(), "{}: the batch mismatched", p.display());
        ensure!(labels.insert(v["batch"].as_str().unwrap_or_default().to_string()), "batch {} given twice", v["batch"]);
        inputs.push(json!({"path": p, "sha256": sha(&b), "batch": v["batch"], "inputs": v["inputs"], "ids": v["ids"], "code": v["code"]}));
        recs.push(v);
    }
    let merged = grammar_a_dryrun::tally::merge(&recs);
    // the population check: every cell's children as the figures file counts them
    let mut differ = Vec::new();
    let empty = serde_json::Map::new();
    let fcells = fig["cells"].as_object().unwrap_or(&empty);
    let keys: BTreeSet<&String> = fcells.keys().chain(merged["cells"].as_object().unwrap_or(&empty).keys()).collect();
    for k in keys {
        for f in ["children", "children_accepted"] {
            let a = fcells.get(k).and_then(|c| c[f].as_u64()).unwrap_or(0);
            let b = merged["cells"][k][f].as_u64().unwrap_or(0);
            if a != b {
                differ.push(json!({"cell": k, "figure": f, "figures_file": a, "counted": b}));
            }
        }
    }
    let rec = json!({"schema": grammar_a_dryrun::COUNTS_SCHEMA, "code": code, "a7_commit": grammar_a_dryrun::A7_COMMIT, "dry_run_commit": grammar_a_dryrun::DRY_RUN_COMMIT,
        "model_calls": renorm::NO_MODEL_CALL, "figures": {"path": figures, "sha256": figures_sha256}, "batches": inputs,
        "population_check": {"cells_compared": "children and children_accepted of every cell, against DRY-RUN-FIGURES.json", "differences": differ},
        "counts": merged, "rules": grammar_a_dryrun::rules()});
    let mut b = serde_json::to_vec_pretty(&rec)?;
    b.push(b'\n');
    ensure!(differ.is_empty(), "{} cells' children differ from DRY-RUN-FIGURES.json; no counts record written", differ.len());
    write_new(out, &b)?;
    Ok(rec)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::overlap::{Overlap, PriorIndex};

    fn read_json(p: &Path) -> Result<Value> {
        Ok(serde_json::from_slice(&fs::read(p)?)?)
    }

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

    fn span_of(p: &str, i: usize) -> (usize, usize) {
        let s = text::sentences(p)[i];
        (s.start, s.end)
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
            decoding: crate::plan::profile("A", 0.7, 0.9, 1536),
            request_sha256: sha(id.as_bytes()),
            seed: 7,
            op: None,
            cohort: None,
            attempt_limit: None,
        }
    }

    fn chat_body(content: &str) -> Value {
        json!({"choices":[{"message":{"content":content},"finish_reason":"stop"}],"usage":{},"timings":{}})
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

    fn sparent(id: &str, domain: &str, t: &str) -> splice::Parent {
        splice::Parent {
            id: id.into(),
            split: "train".into(),
            domain: domain.into(),
            genre: None,
            source_group: format!("g-{id}"),
            writer_key: None,
            cohort_id: "c".into(),
            provenance_sha256: None,
            text: t.into(),
            text_sha256: sha(t.as_bytes()),
            a_tokens: text::words(t),
            words: text::words(t),
        }
    }

    fn sitem(id: &str, parent: &splice::Parent, edits: Vec<splice::Edit>) -> splice::Item {
        splice::Item {
            item_id: id.into(),
            population: "train".into(),
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

    /// A spliced plan, its profile-off journals and its re-verdict: items
    /// `s-two` (two edits, one an insertion), `s-one`, `s-echo` (rejected:
    /// an echo), `s-out` (not in the snapshot), `s-short` (its last request
    /// fails the item check), `s-noend` (an unpunctuated edit in a
    /// list-licensed parent: the two codes give different reasons), `s-dash`
    /// (two spaced hyphens one space apart in a parent with closed em dashes:
    /// amendment 7's code panics), `s-head` (its first request fails, so the
    /// last is judged on its own edit's rules), `s-inc` (incomplete in the
    /// journals) and `s-other` (another plan population, whose edit reuses
    /// `s-one`'s request id with another span). Returns the output
    /// directory, the snapshot and the summary's digest.
    fn spliced_fixture(d: &Path) -> (PathBuf, PathBuf, String) {
        let q1 = format!("{}\n\n{}\n\n{}", prose(&SYN, 1, 5, 11), prose(&SYN, 2, 6, 11), prose(&SYN, 3, 5, 12));
        let q2 = format!("{}\n\n{}", prose(&SYN, 4, 7, 11), prose(&SYN, 5, 7, 12));
        // a parent of 132 words: an item on it fails the window at its item check only
        let q3 = format!("{}\n\n{}", prose(&SYN, 6, 6, 11), prose(&SYN, 7, 6, 11));
        let q4 = format!("{}\n- {}\n- {}\n\n{}", prose(&SYN, 50, 2, 9), prose(&SYN, 51, 1, 6), prose(&SYN, 52, 1, 6), prose(&SYN, 53, 3, 9));
        let q5 = format!("{} The pier stayed open\u{2014}briefly. {}\n\n{} The tide turned\u{2014}slowly. {}", prose(&SYN, 61, 4, 11), prose(&SYN, 62, 3, 11), prose(&SYN, 63, 3, 11), prose(&SYN, 64, 4, 11));
        let (pa, pb, pc) = (sparent("p1", "fiction", &q1), sparent("p2", "news", &q2), sparent("p3", "fiction", &q3));
        let (pd, pe) = (sparent("p4", "web", &q4), sparent("p5", "web", &q5));
        let sw = |p: &str, sp: (usize, usize)| text::words(&p[sp.0..sp.1]);
        let second = |p: &str| {
            let s = p.find(". ").unwrap() + 2;
            (s, s + p[s..].find('.').unwrap() + 1)
        };
        let e_run = span_of(&q1, 7);
        let para_end = span_of(&q1, 4).1;
        let e_one = span_of(&q2, 3);
        let e_echo = span_of(&q1, 12);
        let e_out = span_of(&q2, 9);
        let (e_s1, e_s2) = (span_of(&q3, 1), span_of(&q3, 8));
        let (e_noend, e_dash) = (second(&q4), second(&q5));
        let (e_h1, e_h2) = (span_of(&q3, 3), span_of(&q3, 10));
        let (e_i1, e_i2) = (span_of(&q2, 5), span_of(&q2, 11));
        let mut noend = sedit("e8", "run", e_noend, None, sw(&q4, e_noend));
        noend.end_rule = "paragraph_end".into();
        // s-other's edit (another plan population) reuses e3's id as an insertion at q2's first paragraph end
        let q2_end = span_of(&q2, 6).1;
        let mut other = sitem("s-other", &pb, vec![sedit("e3", "insert", (q2_end, q2_end), Some(" "), 0)]);
        other.population = "train_b".into();
        let items = vec![
            sitem("s-two", &pa, vec![sedit("e1", "run", e_run, None, sw(&q1, e_run)), sedit("e2", "insert", (para_end, para_end), Some(" "), 0)]),
            sitem("s-one", &pb, vec![sedit("e3", "run", e_one, None, sw(&q2, e_one))]),
            other,
            sitem("s-echo", &pa, vec![sedit("e4", "run", e_echo, None, sw(&q1, e_echo))]),
            sitem("s-out", &pb, vec![sedit("e5", "run", e_out, None, sw(&q2, e_out))]),
            sitem("s-short", &pc, vec![sedit("e6", "run", e_s1, None, sw(&q3, e_s1)), sedit("e7", "run", e_s2, None, sw(&q3, e_s2))]),
            sitem("s-noend", &pd, vec![noend]),
            sitem("s-dash", &pe, vec![sedit("e9", "run", e_dash, None, sw(&q5, e_dash))]),
            sitem("s-head", &pc, vec![sedit("e10", "run", e_h1, None, sw(&q3, e_h1)), sedit("e11", "run", e_h2, None, sw(&q3, e_h2))]),
            sitem("s-inc", &pb, vec![sedit("e12", "run", e_i1, None, sw(&q2, e_i1)), sedit("e13", "run", e_i2, None, sw(&q2, e_i2))]),
        ];
        let replies: HashMap<&str, String> = [
            ("e1", prose(&ALT, 21, 1, sw(&q1, e_run))),
            ("e2", prose(&ALT, 22, 1, 12)),
            ("e3", prose(&ALT, 23, 1, sw(&q2, e_one))),
            ("e4", q1[e_echo.0..e_echo.1].replacen(' ', "  ", 1)),
            ("e5", prose(&ALT, 25, 1, sw(&q2, e_out))),
            ("e6", prose(&ALT, 26, 1, sw(&q3, e_s1))),
            ("e7", prose(&ALT, 27, 1, sw(&q3, e_s2))),
            ("e8", "Acorn bramble canyon dune estuary fern glacier heather inlet".to_string()),
            ("e9", "Acorn bramble - - canyon dune estuary fern glacier heather inlet.".to_string()),
            ("e10", q3[e_h1.0..e_h1.1].replacen(' ', "  ", 1)),
            ("e11", prose(&ALT, 28, 1, sw(&q3, e_h2))),
            ("e12", prose(&ALT, 29, 1, sw(&q2, e_i1))),
            ("e13", prose(&ALT, 30, 1, sw(&q2, e_i2))),
        ]
        .into();
        let pdir = d.join("plan");
        fs::create_dir_all(pdir.join("requests")).unwrap();
        let ib = jsonl(&items.iter().map(|i| serde_json::to_value(i).unwrap()).collect::<Vec<_>>()).unwrap();
        let ps = [&pa, &pb, &pc, &pd, &pe];
        let pbytes = jsonl(&ps.iter().map(|p| serde_json::to_value(p).unwrap()).collect::<Vec<_>>()).unwrap();
        fs::write(pdir.join("items.jsonl"), &ib).unwrap();
        fs::write(pdir.join("parents.jsonl"), &pbytes).unwrap();
        // the plan population's requests (s-other's edit reuses e3's id and has no request line of its own)
        let ours: Vec<&splice::Item> = items.iter().filter(|i| i.population == "train").collect();
        let mut reqs = Vec::new();
        for it in &ours {
            for e in &it.edits {
                reqs.push(request(&e.request_id, &it.item_id, &it.parent_id, Some(e.parent_span), e.target_words));
            }
        }
        let rb = jsonl(&reqs.iter().map(|r| serde_json::to_value(r).unwrap()).collect::<Vec<_>>()).unwrap();
        fs::write(pdir.join("requests/g.jsonl"), &rb).unwrap();
        fs::write(pdir.join("plan.json"), serde_json::to_vec(&json!({"schema": splice::PLAN_SCHEMA, "items": {"sha256": sha(&ib)}, "parents": {"sha256": sha(&pbytes)},
            "request_files": {"g": {"sha256": sha(&rb)}}})).unwrap()).unwrap();
        let ov = Overlap::new(PriorIndex::empty());
        let edits: HashMap<String, splice::Edit> = ours.iter().flat_map(|i| i.edits.iter().map(|e| (e.request_id.clone(), e.clone()))).collect();
        let parent_of = |r: &Request| match r.parent_id.as_str() {
            "p1" => q1.as_str(),
            "p2" => q2.as_str(),
            "p3" => q3.as_str(),
            "p4" => q4.as_str(),
            _ => q5.as_str(),
        };
        let mut lines = Vec::new();
        for r in &reqs {
            // s-inc's second request was never generated
            if r.request_id == "e13" {
                continue;
            }
            let parsed = chat_body(&replies[r.request_id.as_str()]);
            let v = splice::verdict(&edits[&r.request_id], parent_of(r), &ov, 200, &parsed);
            assert_eq!(v["accepted"], true, "{}: {v}", r.request_id);
            lines.push(json!({"kind":"intent","request_id":r.request_id,"item_id":r.item_id,"split":r.split,"generator":r.generator,"attempt":0,"seed":r.seed,"t":1,"body":{"prompt":r.prompt}}));
            lines.push(json!({"kind":"response","request_id":r.request_id,"item_id":r.item_id,"split":r.split,"generator":r.generator,"attempt":0,"seed":r.seed,
                "t_start":1,"t_end":2,"elapsed_ms":1,"status":200,"raw_body":serde_json::to_string(&parsed).unwrap(),"timings":{},"usage":{},
                "verdict":v}));
        }
        let runs = d.join("runs");
        fs::create_dir_all(runs.join("g")).unwrap();
        fs::write(runs.join("g/journal-1-w0.jsonl"), jsonl(&lines).unwrap()).unwrap();
        let b = renorm::spliced_batch(&pdir, "train", POPULATION).unwrap();
        let out = d.join("spliced-train");
        renorm::run_batch(&b, &runs, &out).unwrap();
        // the snapshot: R0's items (s-out is not one), with an unparsable dev_select line
        let snap = d.join("snapshot.jsonl");
        let mut sb = jsonl(&["s-two", "s-one", "s-echo", "s-short", "s-noend", "s-dash", "s-head", "s-inc"].iter().map(|i| json!({"id": i, "split": "train"})).collect::<Vec<_>>()).unwrap();
        sb.extend_from_slice(b"{\"split\":\"dev_select\",\"id\":\x00 never parsed\n");
        fs::write(&snap, sb).unwrap();
        let summary_sha = file_sha(&out.join("RENORM-population-train.json")).unwrap();
        (out, snap, summary_sha)
    }

    /// Lines of every split but Train that no reader may parse: each is
    /// broken JSON after its split key.
    const NOT_TRAIN: &[u8] = b"{\"split\":\"cal\",\"id\":\x00 never parsed\n{\"split\":\"dev_iter\", broken\n{\"split\" : \"dev_select\", broken\n{\"split\":\"test\",\x00\n";

    #[test]
    fn a_spliced_batch_is_counted_from_its_records_with_every_child_identical() {
        engine::quiet_panics();
        let dir = tempfile::tempdir().unwrap();
        let (out, snap, h) = spliced_fixture(dir.path());
        let sh = file_sha(&snap).unwrap();
        let c = count_spliced("spliced-train", &out, &h, &snap, &sh).unwrap();
        assert_eq!(c.tally.mismatched(), 0, "{:?}", c.tally.mismatches);
        // R0's complete items only: s-out is not in the snapshot, s-inc is incomplete (its first request is not counted)
        assert_eq!(c.children["children"], 10, "{}", c.children);
        assert_eq!(c.children["items"]["items_not_r0_records"], 1);
        assert_eq!(c.children["items"]["r0_items_not_complete"], 1);
        let totals = grammar_a_dryrun::tally::totals(&c.tally.cells);
        let all = &totals["all"];
        // s-short's last request fails its item check (the window on the final document), its first passes;
        // s-head's first fails (an echo), so its last is judged on its own edit and passes
        assert_eq!(all.get("children_accepted").copied(), Some(6), "{all:?}");
        assert_eq!(all.get("children|insertion").copied(), Some(1));
        assert!(c.tally.cells.contains_key("spliced|mixed|fiction") && c.tally.cells.contains_key("spliced|mixed|news"));
        assert_eq!(c.inputs["snapshot"]["lines_of_other_splits_skipped_unparsed"]["dev_select"], 1);
        assert_eq!(c.inputs["snapshot"]["sha256"], sh.as_str());
        // count 1: s-noend's reasons differ (the landed code's paragraph reason), s-dash's amendment-7 run panics
        let r = &c.tally.reasons["spliced|mixed"];
        assert_eq!(all.get("c1_reasons_differ").copied(), Some(1), "{all:?}");
        assert_eq!(r.get("c1_reasons_differ:landed_only:form_paragraph_no_sentence_end").copied(), Some(1), "{r:?}");
        assert_eq!(all.get("c1_a7_failed").copied(), Some(1), "{all:?}");
        assert_eq!(r.get("c1_a7_failed:panic:conventions.rs:389").copied(), Some(1), "{r:?}");
        let changes: u64 = ["c1_both_accepted", "c1_both_rejected_same_reasons", "c1_accepted_to_rejected", "c1_rejected_to_accepted", "c1_reasons_differ", "c1_a7_failed"]
            .iter()
            .map(|k| all.get(*k).copied().unwrap_or(0))
            .sum();
        assert_eq!(changes, 10);
        // the batch record and the ids-only file are new files of mode 600
        let rec = write_batch(&dir.path().join("COUNTS.json"), &dir.path().join("ids.jsonl"), "spliced-train", &c, &json!({"commit": "test"})).unwrap();
        assert_eq!(rec["schema"], grammar_a_dryrun::BATCH_SCHEMA);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(fs::metadata(dir.path().join("COUNTS.json")).unwrap().permissions().mode() & 0o777, 0o600);
        }
        assert!(write_batch(&dir.path().join("COUNTS.json"), &dir.path().join("ids2.jsonl"), "spliced-train", &c, &json!({})).is_err(), "an existing path is refused");
        // a summary of another digest, and a snapshot of another digest, are refused before anything is read
        assert!(count_spliced("spliced-train", &out, &"0".repeat(64), &snap, &sh).err().unwrap().to_string().contains("is not the recorded"));
        assert!(count_spliced("spliced-train", &out, &h, &snap, &"0".repeat(64)).err().unwrap().to_string().contains("is not the bound"));
        // deliberately unparsable lines of every other split in the output journal are read past unparsed
        let jp = out.join("g/journal-renorm.jsonl");
        let mut f = fs::OpenOptions::new().append(true).open(&jp).unwrap();
        std::io::Write::write_all(&mut f, b"{\"kind\":\"response\",\"split\":\"dev_select\",\"request_id\":\"e1\",\x00 never parsed\n{\"kind\":\"response\",\"split\":\"cal\",\"request_id\":\"e3\",\x00\n").unwrap();
        std::io::Write::write_all(&mut f, NOT_TRAIN).unwrap();
        drop(f);
        let h2 = rebind(&out);
        let c2 = count_spliced("spliced-train", &out, &h2, &snap, &sh).unwrap();
        assert_eq!(c2.tally.cells, c.tally.cells);
        assert_eq!(c2.children["journals"]["lines_skipped_unparsed"], 6, "{}", c2.children);
        // and in the plan's items, parents and request files (their digests re-bound in the plan and the summary)
        let pdir = PathBuf::from(read_json(&out.join("RENORM-population-train.json")).unwrap()["inputs"]["plan"].as_str().unwrap());
        for f in ["items.jsonl", "parents.jsonl", "requests/g.jsonl"] {
            let mut o = fs::OpenOptions::new().append(true).open(pdir.join(f)).unwrap();
            std::io::Write::write_all(&mut o, NOT_TRAIN).unwrap();
        }
        let h3 = rebind_plan(&out, &pdir);
        let c3 = count_spliced("spliced-train", &out, &h3, &snap, &sh).unwrap();
        assert_eq!(c3.tally.cells, c.tally.cells);
        assert_eq!(c3.tally.reasons, c.tally.reasons);
        for (input, key) in [("items", "items"), ("parents", "parents"), ("requests", "requests")] {
            for split in ["cal", "dev_iter", "dev_select", "test"] {
                assert_eq!(c3.inputs["plan_lines_skipped_unparsed"][input][split], 1, "{key} {split}: {}", c3.inputs["plan_lines_skipped_unparsed"]);
            }
        }
        assert_eq!(c3.inputs["plan_items_lines_skipped_unparsed"]["cal"], 1);
    }

    /// Re-binds the plan's file digests in `plan.json` and the summary's
    /// inputs to the plan's current files; returns the summary's new digest.
    fn rebind_plan(out: &Path, pdir: &Path) -> String {
        let pj = pdir.join("plan.json");
        let mut m = read_json(&pj).unwrap();
        m["items"]["sha256"] = json!(file_sha(&pdir.join("items.jsonl")).unwrap());
        m["parents"]["sha256"] = json!(file_sha(&pdir.join("parents.jsonl")).unwrap());
        m["request_files"]["g"]["sha256"] = json!(file_sha(&pdir.join("requests/g.jsonl")).unwrap());
        fs::write(&pj, serde_json::to_vec(&m).unwrap()).unwrap();
        let sp = out.join("RENORM-population-train.json");
        let mut s = read_json(&sp).unwrap();
        s["inputs"]["plan_sha256"] = json!(file_sha(&pj).unwrap());
        s["inputs"]["items_sha256"] = m["items"]["sha256"].clone();
        s["inputs"]["parents_sha256"] = m["parents"]["sha256"].clone();
        s["inputs"]["request_files"]["g"] = m["request_files"]["g"]["sha256"].clone();
        fs::write(&sp, serde_json::to_vec_pretty(&s).unwrap()).unwrap();
        file_sha(&sp).unwrap()
    }

    /// Re-binds a spliced batch's summary to its current output files;
    /// returns the summary's new digest.
    fn rebind(out: &Path) -> String {
        let sp = out.join("RENORM-population-train.json");
        let mut s = read_json(&sp).unwrap();
        s["outputs"]["reverdict"]["sha256"] = json!(file_sha(&out.join("reverdict-population-train.jsonl")).unwrap());
        for j in s["outputs"]["journals"].as_array_mut().unwrap() {
            j["sha256"] = json!(file_sha(&out.join(j["path"].as_str().unwrap())).unwrap());
        }
        fs::write(&sp, serde_json::to_vec_pretty(&s).unwrap()).unwrap();
        file_sha(&sp).unwrap()
    }

    #[test]
    fn a_spliced_item_sharing_a_hard_wrapped_line_is_laid_out_in_parent_order_and_counted() {
        // Amendment 29 item 29.1 through renorm and the count: the later
        // edit's laid-out text is the item's (its column from the earlier
        // edit's text), renorm records it in sequential judgement, and the
        // count re-derives it identically (no mismatch).
        use grammar_a_form::conventions::{Choice, Conventions};
        use grammar_a_form::profile::{EditV2, item_v2};
        engine::quiet_panics();
        fn none() -> Conventions {
            fn c<K>() -> Choice<K> {
                Choice { value: None, source: "none" }
            }
            Conventions { double: c(), single: c(), dash: c(), range: c(), ellipsis: c() }
        }
        let mut found = None;
        'seed: for seed in 0..400u64 {
            let paras: Vec<String> = (0..4).map(|k| grammar_a_form::transfer::wrap(&prose(&SYN, seed * 10 + k, 9, 5), 60, 0).join("\n")).collect();
            let q = paras.join("\n\n");
            let ss: Vec<(usize, usize)> = text::sentences(&q).iter().map(|s| (s.start, s.end)).collect();
            // a third edit in the last paragraph shares no line with them
            let last_para = q.rfind("\n\n").unwrap();
            let sc = *ss.last().unwrap();
            for a in 0..ss.len() {
                for b in a + 2..ss.len() {
                    if q[ss[a].1..ss[b].0].contains('\n') || ss[b].1 > last_para {
                        continue;
                    }
                    let (r1, r2, r3) = (prose(&ALT, seed * 10 + 5, 1, 5), prose(&ALT, seed * 10 + 6, 1, 5), prose(&ALT, seed * 10 + 7, 1, 5));
                    let e = [EditV2 { span: ss[a], insert_sep: None, raw: &r1 }, EditV2 { span: ss[b], insert_sep: None, raw: &r2 }, EditV2 { span: sc, insert_sep: None, raw: &r3 }];
                    let it = item_v2(&q, &none(), &e);
                    if it.reasons.is_empty() && it.reps[1] != item_v2(&q, &none(), &e[1..2]).reps[0] {
                        found = Some((q.clone(), [ss[a], ss[b], sc], [r1, r2, r3], it.reps.clone()));
                        break 'seed;
                    }
                }
            }
        }
        // three requests: the middle one is judged before the last, so its
        // text comes from renorm's layout context, not the item check
        let (q, spans, raws, reps) = found.expect("a shared line whose later edit's column changes");
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        let p = sparent("p1", "fiction", &q);
        let sw = |sp: (usize, usize)| text::words(&q[sp.0..sp.1]);
        let item = sitem("s-wrap", &p, (0..3).map(|k| sedit(&format!("w{}", k + 1), "run", spans[k], None, sw(spans[k]))).collect());
        let pdir = d.join("plan");
        fs::create_dir_all(pdir.join("requests")).unwrap();
        let ib = jsonl(&[serde_json::to_value(&item).unwrap()]).unwrap();
        let pbytes = jsonl(&[serde_json::to_value(&p).unwrap()]).unwrap();
        fs::write(pdir.join("items.jsonl"), &ib).unwrap();
        fs::write(pdir.join("parents.jsonl"), &pbytes).unwrap();
        let reqs: Vec<Request> = item.edits.iter().map(|e| request(&e.request_id, &item.item_id, "p1", Some(e.parent_span), e.target_words)).collect();
        let rb = jsonl(&reqs.iter().map(|r| serde_json::to_value(r).unwrap()).collect::<Vec<_>>()).unwrap();
        fs::write(pdir.join("requests/g.jsonl"), &rb).unwrap();
        fs::write(pdir.join("plan.json"), serde_json::to_vec(&json!({"schema": splice::PLAN_SCHEMA, "items": {"sha256": sha(&ib)}, "parents": {"sha256": sha(&pbytes)},
            "request_files": {"g": {"sha256": sha(&rb)}}})).unwrap()).unwrap();
        let ov = Overlap::new(PriorIndex::empty());
        let mut lines = Vec::new();
        for (r, (e, raw)) in reqs.iter().zip(item.edits.iter().zip(&raws)) {
            let parsed = chat_body(raw);
            let v = splice::verdict(e, &q, &ov, 200, &parsed);
            assert_eq!(v["accepted"], true, "{}: {v}", r.request_id);
            lines.push(json!({"kind":"intent","request_id":r.request_id,"item_id":r.item_id,"split":r.split,"generator":r.generator,"attempt":0,"seed":r.seed,"t":1,"body":{"prompt":r.prompt}}));
            lines.push(json!({"kind":"response","request_id":r.request_id,"item_id":r.item_id,"split":r.split,"generator":r.generator,"attempt":0,"seed":r.seed,
                "t_start":1,"t_end":2,"elapsed_ms":1,"status":200,"raw_body":serde_json::to_string(&parsed).unwrap(),"timings":{},"usage":{},"verdict":v}));
        }
        let runs = d.join("runs");
        fs::create_dir_all(runs.join("g")).unwrap();
        fs::write(runs.join("g/journal-1-w0.jsonl"), jsonl(&lines).unwrap()).unwrap();
        let b = renorm::spliced_batch(&pdir, "train", POPULATION).unwrap();
        let out = d.join("spliced-train");
        renorm::run_batch(&b, &runs, &out).unwrap();
        // renorm's records hold the item's layout for both requests
        let rv: Value = serde_json::from_str(fs::read_to_string(out.join("reverdict-population-train.jsonl")).unwrap().lines().next().unwrap()).unwrap();
        assert_eq!(rv["pass"], true, "{rv}");
        for (k, q) in rv["requests"].as_array().unwrap().iter().enumerate() {
            assert_eq!(q["text_sha256"], sha(reps[k].as_bytes()).as_str(), "request {k}");
        }
        let snap = d.join("snapshot.jsonl");
        fs::write(&snap, jsonl(&[json!({"id": "s-wrap", "split": "train"})]).unwrap()).unwrap();
        let h = file_sha(&out.join("RENORM-population-train.json")).unwrap();
        let c = count_spliced("spliced-train", &out, &h, &snap, &file_sha(&snap).unwrap()).unwrap();
        assert_eq!(c.tally.mismatched(), 0, "{:?}", c.tally.mismatches);
        assert_eq!(c.children["children"], 3);
        let j = c.tally.to_json();
        assert_eq!(j["gate"]["count4_accepted_wrapped_lines_over_budget"]["gate"], 0, "{}", j["gate"]);
    }

    #[test]
    fn a_recorded_child_whose_laid_out_text_was_altered_stops_the_run() {
        let dir = tempfile::tempdir().unwrap();
        let (out, snap, _) = spliced_fixture(dir.path());
        // alter s-one's laid-out text in the output journal and its record, and re-bind the summary to both
        let jp = out.join("g/journal-renorm.jsonl");
        let mut new_text = String::new();
        let lines: Vec<Value> = fs::read_to_string(&jp)
            .unwrap()
            .lines()
            .map(|l| {
                let mut v: Value = serde_json::from_str(l).unwrap();
                if v["request_id"] == "e3" && v["renorm"].is_object() {
                    new_text = format!("{} Quartz.", v["verdict"]["text"].as_str().unwrap());
                    v["verdict"]["text"] = json!(new_text);
                }
                v
            })
            .collect();
        fs::write(&jp, jsonl(&lines).unwrap()).unwrap();
        let rp = out.join("reverdict-population-train.jsonl");
        let recs: Vec<Value> = fs::read_to_string(&rp)
            .unwrap()
            .lines()
            .map(|l| {
                let mut v: Value = serde_json::from_str(l).unwrap();
                if v["item_id"] == "s-one" {
                    v["requests"][0]["text_sha256"] = json!(sha(new_text.as_bytes()));
                }
                v
            })
            .collect();
        fs::write(&rp, jsonl(&recs).unwrap()).unwrap();
        let c = count_spliced("spliced-train", &out, &rebind(&out), &snap, &file_sha(&snap).unwrap()).unwrap();
        assert_eq!(c.tally.mismatches.get("rederived_text_differs").copied(), Some(1), "{:?}", c.tally.mismatches);
        let e = write_batch(&dir.path().join("COUNTS.json"), &dir.path().join("ids.jsonl"), "spliced-train", &c, &json!({})).unwrap_err();
        assert!(e.to_string().contains("the run stops"), "{e}");
        let rec = read_json(&dir.path().join("COUNTS.json")).unwrap();
        assert!(rec["stopped"].is_string());
        let ids = fs::read_to_string(dir.path().join("ids.jsonl")).unwrap();
        assert!(ids.contains("\"s-one\"") && !ids.contains("Quartz"), "{ids}");
        // the merge refuses a stopped batch
        let fig = dir.path().join("figures.json");
        fs::write(&fig, b"{\"cells\":{}}\n").unwrap();
        assert!(merge(&[dir.path().join("COUNTS.json")], &fig, &file_sha(&fig).unwrap(), &dir.path().join("M.json"), &json!({})).is_err());
    }

    fn row(id: &str, kind: &str, cohort: &str, origin: &str, group: &str, domain: &str, t: &str) -> Value {
        let spans = if origin == "model_only" { json!([[0, t.len()]]) } else { json!([]) };
        json!({"id": id, "split": "train", "text": t, "text_sha256": sha(t.as_bytes()), "origin": origin, "source_kind": kind,
            "cohort_id": cohort, "source_group": group, "domain": domain, "ai_byte_spans": spans, "weight": 1.0})
    }

    /// A synthetic email (a greeting and a sign-off around a punctuated
    /// body) and a mirror of it with its own greeting, a list and a sign-off:
    /// amendment 7's code accepts the child and the landed code rejects it
    /// (its list is unlicensed).
    pub(crate) fn email_pair(seed: u64) -> (String, String) {
        let body = (0..3).map(|k| prose(&SYN, seed + k, 4, 14)).collect::<Vec<_>>().join("\n\n");
        let parent = format!("Hi Bob,\n\n{body}\n\nThanks,\nSam\n");
        let child = format!(
            "Hello Bob,\n\n{}\nHere is what we still need before the meeting:\n- the signed contract from the legal team\n- two printed copies of the budget\n- the booking for the large room\n\n{}\n\nBest regards,\nSam",
            prose(&ALT, seed + 10, 5, 14),
            prose(&ALT, seed + 11, 4, 14)
        );
        (parent, child)
    }

    #[test]
    fn a_rows_batch_is_counted_against_its_raid_humans_parents_and_donors() {
        engine::quiet_panics();
        let q1 = format!("{}\n\n{}", prose(&SYN, 31, 7, 11), prose(&SYN, 32, 7, 12));
        // the RAID human uses closed em dashes; its model row has two spaced hyphens one space apart
        let q2 = format!("{} The pier stayed open\u{2014}briefly. {}\n\n{}", prose(&SYN, 33, 6, 11), prose(&SYN, 35, 1, 11), prose(&SYN, 34, 7, 11));
        let gen1 = format!("{} The keeper wrote acorn - - bramble twice. {}", prose(&ALT, 41, 9, 11), prose(&ALT, 43, 8, 11));
        let (em, ec) = email_pair(80);
        let s = span_of(&q1, 3);
        let with = prose(&ALT, 42, 1, 11);
        let mixed = format!("{}{}{}", &q1[..s.0], with, &q1[s.1..]);
        let mspan = (s.0, mixed.len() - (q1.len() - s.1));
        let mut mrow = row("stage3:mx", "stage3_legacy", "stage3_legacy_train", "mixed", "g-p", "fiction", &mixed);
        mrow["ai_byte_spans"] = json!([[mspan.0, mspan.1]]);
        let rows_v = vec![
            row("raid-h1", "external_pool", "raid-v1-paired", "human_only", "raid_source:1", "raid_books", &q2),
            row("raid-g1", "external_pool", "raid-v1-paired", "model_only", "raid_source:1", "raid_books", &gen1),
            row("stage3:p1", "stage3_legacy", "stage3_legacy_train", "human_only", "g-p", "fiction", &q1),
            mrow,
            row("stage3:m1", "stage3_legacy", "stage3_legacy_train", "model_only", "g-m", "fiction", &prose(&ALT, 44, 17, 11)),
            row("stage3:h2", "stage3_legacy", "stage3_legacy_train", "human_only", "g-h", "fiction", &format!("{}\n\n{}", prose(&SYN, 36, 6, 11), prose(&SYN, 37, 7, 11))),
            // an email donor and a model row laid out like it (the only mail domain human)
            row("stage3:hm", "stage3_legacy", "stage3_legacy_train", "human_only", "g-hm", "mail", &em),
            row("stage3:mm", "stage3_legacy", "stage3_legacy_train", "model_only", "g-mm", "mail", &ec),
        ];
        let dir = tempfile::tempdir().unwrap();
        let rows_p = dir.path().join("rows.jsonl");
        let mut rb = jsonl(&rows_v).unwrap();
        rb.extend_from_slice(NOT_TRAIN);
        fs::write(&rows_p, rb).unwrap();
        let doc = json!({"schema": "slop_ninja_a_pool_document_v1", "id": "raid-h1", "origin": "human_only", "source_group": "raid_source:1", "domain": "raid_books",
            "cohort_id": "raid-v1-paired", "intended_use": "train_only", "split_assigned": false, "text": q2, "text_sha256": sha(q2.as_bytes()),
            "raid": {"attack": {"name": "none", "attacked": false}}});
        let docs_p = dir.path().join("raid.jsonl");
        fs::write(&docs_p, jsonl(&[doc]).unwrap()).unwrap();
        let out = dir.path().join("rows-train");
        renorm::renorm_rows(&rows_p, None, Some(&docs_p), POPULATION, &out, false).unwrap();
        let h = file_sha(&out.join("ROWS-RENORM.json")).unwrap();
        let c = count_rows("rows-train", &out, &h).unwrap();
        assert_eq!(c.tally.mismatched(), 0, "{:?}", c.tally.mismatches);
        // a RAID row (against its RAID human), a mixed stage-3 row (its recovered parent) and two model-only ones (donors)
        assert_eq!(c.children["children"], 4, "{}", c.children);
        let totals = grammar_a_dryrun::tally::totals(&c.tally.cells);
        assert_eq!(totals["external_pool"]["children"], 1);
        assert_eq!(totals["stage3_legacy"]["children"], 3);
        assert_eq!(totals["all"].get("children|donor").copied(), Some(2));
        assert_eq!(totals["all"].get("children|mixed").copied(), Some(1));
        for split in ["cal", "dev_iter", "dev_select", "test"] {
            assert_eq!(c.inputs["rows_lines_of_other_splits_skipped_unparsed"][split], 1, "{split}");
        }
        // count 1: the email row flips (accepted under amendment 7's code, rejected under the landed code, by its
        // reasons); the RAID row's amendment-7 run panics
        let mail = &c.tally.cells["stage3_legacy|model_only|mail"];
        assert_eq!(mail.get("c1_accepted_to_rejected").copied(), Some(1), "{mail:?}");
        let r = &c.tally.reasons["stage3_legacy|model_only"];
        assert_eq!(r.get("c1_accepted_to_rejected:form_line_list_like").copied(), Some(1), "{r:?}");
        let raid = &c.tally.cells["external_pool|model_only|raid_books"];
        assert_eq!(raid.get("c1_a7_failed").copied(), Some(1), "{raid:?}");
        assert_eq!(c.tally.reasons["external_pool|model_only"].get("c1_a7_failed:panic:conventions.rs:389").copied(), Some(1));
        // a span row whose recorded final document's digest is not the spliced one is a mismatch
        let rp = out.join("rows-reverdict.jsonl");
        let lines: Vec<Value> = fs::read_to_string(&rp)
            .unwrap()
            .lines()
            .map(|l| {
                let mut v: Value = serde_json::from_str(l).unwrap();
                if v["id"] == "stage3:mx" {
                    v["text_sha256"] = json!("0".repeat(64));
                }
                v
            })
            .collect();
        fs::write(&rp, jsonl(&lines).unwrap()).unwrap();
        let sp = out.join("ROWS-RENORM.json");
        let mut sv = read_json(&sp).unwrap();
        sv["outputs"]["reverdict"]["sha256"] = json!(file_sha(&rp).unwrap());
        fs::write(&sp, serde_json::to_vec_pretty(&sv).unwrap()).unwrap();
        let c2 = count_rows("rows-train", &out, &file_sha(&sp).unwrap()).unwrap();
        assert_eq!(c2.tally.mismatches.get("final_document_differs").copied(), Some(1), "{:?}", c2.tally.mismatches);
    }
}
