//! Resumable local generation against one llama-server, with durable
//! append-only journals (intent before send, response after), bounded
//! attempts and the protocol acceptance checks.
use crate::overlap::{self, Overlap};
use crate::plan::Request;
use crate::text;
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashMap},
    fs::{self, File, OpenOptions},
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, atomic::{AtomicUsize, Ordering}},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tokenizers::Tokenizer;

pub const MAX_A_TOKENS: usize = 2048;
pub const ATTEMPTS: u32 = 2;

pub fn now_ms() -> u128 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis()
}

/// Journal durability: every line is appended (one write call) immediately;
/// `fdatasync` runs at most every SYNC_INTERVAL and when the journal closes.
/// A crash can lose at most the last interval of lines; their requests are
/// then re-run on resume (local generation has no external side effects).
pub const JOURNAL_SYNC_POLICY: &str = "append_each_line_fdatasync_at_most_every_5s_and_on_close";
pub const SYNC_INTERVAL: Duration = Duration::from_secs(5);

pub struct Journal {
    f: File,
    last_sync: Instant,
    dirty: bool,
}
impl Journal {
    pub fn open(path: &Path) -> Result<Self> {
        Ok(Journal { f: OpenOptions::new().create(true).append(true).open(path)?, last_sync: Instant::now(), dirty: false })
    }
    pub fn write(&mut self, v: &Value) -> Result<()> {
        let mut b = serde_json::to_vec(v)?;
        b.push(b'\n');
        self.f.write_all(&b)?;
        self.dirty = true;
        if self.last_sync.elapsed() >= SYNC_INTERVAL {
            self.f.sync_data()?;
            self.last_sync = Instant::now();
            self.dirty = false;
        }
        Ok(())
    }
}
impl Drop for Journal {
    fn drop(&mut self) {
        if self.dirty {
            let _ = self.f.sync_data();
        }
    }
}

/// State of one request reconstructed from journals.
#[derive(Default, Debug, Clone)]
pub struct ReqState {
    pub intents: Vec<u32>,
    pub responses: BTreeMap<u32, Value>,
    /// The attempt limit its intent lines carry (`attempt_limit`: written
    /// only for a request that carries one, B5b's regenerated mirrors; R0's
    /// requests and their journals have none).
    pub limit: Option<u32>,
}
impl ReqState {
    pub fn accepted(&self) -> Option<&Value> {
        self.responses.values().find(|r| r["verdict"]["accepted"] == true)
    }
    /// Attempts used: every intent (an intent without a response is an
    /// uncertain, never-replayed attempt).
    pub fn used(&self) -> u32 {
        self.intents.iter().copied().max().map(|m| m + 1).unwrap_or(0)
    }
    /// Done under the limit its journal carries (`limit`), else R0's
    /// (`ATTEMPTS`): for readers that hold journals but not the request
    /// (`rates`, `stratify-train`). A reader that holds the request uses
    /// `done_within(Request::attempts())`.
    pub fn done(&self) -> bool {
        self.done_within(self.limit.unwrap_or(ATTEMPTS))
    }
    /// Records one journal line's kind and attempt (and an intent's carried
    /// limit, which every intent of the request must give alike).
    fn add(&mut self, v: Value, a: u32) -> Result<()> {
        match v["kind"].as_str() {
            Some("intent") => {
                if let Some(l) = v.get("attempt_limit") {
                    let l = l.as_u64().context("attempt_limit")? as u32;
                    ensure!(self.limit.is_none_or(|x| x == l), "request {}: its intents carry different attempt limits", v["request_id"]);
                    self.limit = Some(l);
                }
                self.intents.push(a)
            }
            Some("response") => {
                self.responses.insert(a, v);
            }
            _ => bail!("journal kind"),
        }
        Ok(())
    }
    /// Done under `limit` attempts counted from the first: accepted, or
    /// `limit` attempts used (B5b; ruling R6). A request's limit is
    /// `Request::attempts`.
    pub fn done_within(&self, limit: u32) -> bool {
        self.accepted().is_some() || self.used() >= limit
    }
}

pub fn journal_files(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut v = Vec::new();
    if dir.exists() {
        for e in fs::read_dir(dir)? {
            let p = e?.path();
            if p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("journal-") && n.ends_with(".jsonl")) {
                v.push(p);
            }
        }
    }
    v.sort();
    Ok(v)
}

pub fn load_states(dirs: &[PathBuf]) -> Result<HashMap<String, ReqState>> {
    let mut m: HashMap<String, ReqState> = HashMap::new();
    for d in dirs {
        for p in journal_files(d)? {
            let f = BufReader::new(File::open(&p)?);
            for (n, line) in f.lines().enumerate() {
                let line = line?;
                if line.trim().is_empty() {
                    continue;
                }
                let v: Value = match serde_json::from_str(&line) {
                    Ok(v) => v,
                    Err(e) => {
                        // A torn final line from a crash: keep reading the rest.
                        eprintln!("torn journal line {}:{}: {e}", p.display(), n + 1);
                        continue;
                    }
                };
                let id = v["request_id"].as_str().context("request_id")?.to_owned();
                let a = v["attempt"].as_u64().context("attempt")? as u32;
                m.entry(id).or_default().add(v, a)?;
            }
        }
    }
    Ok(m)
}

/// Journal lines a split's reader skipped unparsed: by split, and those with
/// no readable split (torn lines among them).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SkippedLines {
    pub other_splits: BTreeMap<String, usize>,
    pub unreadable: usize,
}

/// [`load_states`] over the journal lines of one split only: each line's
/// split keys are read from its bytes before it is parsed
/// (`renorm::line_split`, amendment 15 item 15.4's reading). A line is
/// parsed when it names `split`, or names no split and its `request_id`,
/// read from its bytes, is one of `ids` (renorm's `SPLIT_RULE`); every other
/// line (another split, an unreadable split, a torn line) is skipped
/// unparsed and counted. Used where a reader must never parse old Dev-select
/// or consumed-Test lines (protocol A2 §2.4; B5b).
pub fn load_states_of_split(dirs: &[PathBuf], split: &str, ids: &std::collections::HashSet<String>) -> Result<(HashMap<String, ReqState>, SkippedLines)> {
    let mut m: HashMap<String, ReqState> = HashMap::new();
    let mut sk = SkippedLines::default();
    for d in dirs {
        for p in journal_files(d)? {
            let f = BufReader::new(File::open(&p)?);
            for (n, line) in f.split(b'\n').enumerate() {
                let line = line?;
                if line.iter().all(u8::is_ascii_whitespace) {
                    continue;
                }
                match crate::renorm::line_split(&line, "split") {
                    crate::renorm::LineSplit::One(s) if s == split => {}
                    crate::renorm::LineSplit::One(s) => {
                        *sk.other_splits.entry(s).or_default() += 1;
                        continue;
                    }
                    crate::renorm::LineSplit::Missing
                        if matches!(crate::renorm::line_split(&line, "request_id"), crate::renorm::LineSplit::One(r) if ids.contains(&r)) => {}
                    _ => {
                        sk.unreadable += 1;
                        continue;
                    }
                }
                let v: Value = match serde_json::from_slice(&line) {
                    Ok(v) => v,
                    Err(e) => {
                        eprintln!("torn journal line {}:{}: {e}", p.display(), n + 1);
                        continue;
                    }
                };
                let id = v["request_id"].as_str().context("request_id")?.to_owned();
                let a = v["attempt"].as_u64().context("attempt")? as u32;
                m.entry(id).or_default().add(v, a)?;
            }
        }
    }
    Ok((m, sk))
}

/// Result of a [`PostProcess`] step: the text that every later acceptance
/// check reads (and the journal keeps), extra rejection reasons, and a record
/// kept in the verdict diagnostics as `post`.
pub struct PostOut {
    pub text: String,
    pub reasons: Vec<String>,
    pub record: Value,
}

/// Optional deterministic post-processing of a cleaned response, run inside
/// the acceptance path before the standard checks (for example a
/// parent-conditioned format normaliser). It must not call any model or
/// network service. The text of a span or edit request must be trimmed,
/// because assembly splices it between human characters. A whole-document
/// (mirror) text may keep edge whitespace: profile v2's layout transfer gives
/// a mirror its parent's head and tail whitespace and line leads, so the
/// journal keeps that text as accepted and its consumers must not trim it.
/// When the step is profile v2's (`is_profile_v2`), the verdict also keeps
/// the cleaned response it read as `raw_text` (the raw child that profile
/// v2's raw twin and item check read). Any other step leaves the verdict as
/// it was before profile v2 existed, so v1 journals keep their bytes.
pub trait PostProcess: Send + Sync {
    fn apply(&self, r: &Request, parent: Option<&str>, clean: &str) -> PostOut;
    /// Whether this step is profile v2's (`cc_cohort_profile_v2`). Only then
    /// does a verdict keep `raw_text`. Every step states it; there is no
    /// default.
    fn is_profile_v2(&self) -> bool;
}

pub struct Ctx {
    pub endpoint: String,
    pub alias: String,
    pub config: Value,
    pub tokenizer: Tokenizer,
    pub overlap: Overlap,
    pub parents: HashMap<String, String>,
    pub client: reqwest::blocking::Client,
    /// Post-processing applied to every cleaned response before the checks.
    pub post: Option<Arc<dyn PostProcess>>,
    /// Final-document word window (inclusive, whitespace words): the
    /// single-span assembly (the response itself for a whole-document
    /// request) must fall inside it. None leaves length to the per-request
    /// target check alone.
    pub doc_words: Option<(usize, usize)>,
}

/// Rejection reasons of the final-document word window, if any: the window
/// `cc_final_length_window_v1`'s reasons, from `grammar_a_form::window`.
pub fn doc_window_reasons(words: usize, window: (usize, usize)) -> Vec<String> {
    grammar_a_form::window::reasons(words, window)
}

pub fn a_tokens(tok: &Tokenizer, s: &str) -> Result<usize> {
    let e = tok.encode(s, true).map_err(|e| anyhow::anyhow!(e.to_string()))?;
    Ok(e.get_ids().len())
}

/// HTTP body for one attempt.
pub fn body(ctx: &Ctx, r: &Request, seed: u32) -> Value {
    let d = &r.decoding;
    let cfg = &ctx.config;
    if r.endpoint == "completion" {
        json!({"prompt":r.prompt,"n_predict":d.max_tokens,"temperature":d.temperature,"top_p":d.top_p,"top_k":d.top_k,
            "min_p":d.min_p,"repeat_penalty":d.repeat_penalty,"seed":seed,"stream":false,"cache_prompt":false})
    } else {
        // The pinned fix-slop bundle is about 5k tokens: keep prompt plus
        // completion inside one slot's context for reasoning models too.
        let max_tokens = match (cfg["max_tokens_override"].as_u64(), r.op.as_deref()) {
            (Some(o), Some("fix_slop")) => o.min(2304),
            (Some(o), _) => o,
            (None, _) => d.max_tokens as u64,
        };
        let mut b = json!({"model":ctx.alias,"messages":[{"role":"user","content":r.prompt}],"temperature":d.temperature,
            "top_p":d.top_p,"top_k":d.top_k,"min_p":d.min_p,"repeat_penalty":d.repeat_penalty,"max_tokens":max_tokens,
            "seed":seed,"stream":false});
        b["chat_template_kwargs"] = cfg["chat_template_kwargs"].clone();
        b["reasoning_format"] = cfg["reasoning_format"].clone();
        b
    }
}

/// Splice replacement(s) into the source; returns final text, final byte
/// spans of the replacements and the realised final-word share. A
/// replacement is non-empty with no trailing whitespace, and leading
/// whitespace only as one line break and a lead (protocol A2 amendment 29
/// item 29.1), which its final span excludes.
pub fn assemble(source: &str, spans: &[(usize, usize)], reps: &[&str]) -> Result<(String, Vec<(usize, usize)>, f64)> {
    ensure!(spans.len() == reps.len() && !spans.is_empty(), "span/replacement count");
    let mut out = String::new();
    let mut fin = Vec::new();
    let mut cur = 0;
    let mut rw = 0;
    for (&(s, e), r) in spans.iter().zip(reps) {
        ensure!(s >= cur && s < e && e <= source.len(), "span order");
        ensure!(source.is_char_boundary(s) && source.is_char_boundary(e), "char boundary");
        ensure!(s == 0 || source[..s].ends_with(char::is_whitespace), "left token boundary");
        ensure!(e == source.len() || source[e..].starts_with(char::is_whitespace), "right token boundary");
        // a replacement may begin with one line break and a lead (protocol A2
        // amendment 29 item 29.1), outside its final span
        let lead = r.len() - r.trim_start().len();
        ensure!(!r.trim().is_empty() && r.trim_end() == *r && (lead == 0 || (r.starts_with('\n') && !r[1..lead].contains('\n'))), "replacement layout");
        out.push_str(&source[cur..s]);
        let fs = out.len() + lead;
        out.push_str(r);
        fin.push((fs, out.len()));
        rw += text::words(r);
        cur = e;
    }
    out.push_str(&source[cur..]);
    let total = text::words(&out);
    let human: usize = {
        let mut h = 0;
        let mut c = 0;
        for &(s, e) in spans {
            h += text::words(&source[c..s]);
            c = e;
        }
        h + text::words(&source[c..])
    };
    ensure!(total == rw + human && total > 0, "word accounting");
    Ok((out, fin, rw as f64 / total as f64))
}

/// Acceptance verdict for one response.
pub fn verdict(ctx: &Ctx, r: &Request, status: u16, parsed: &Value) -> Value {
    let mut reasons: Vec<String> = Vec::new();
    let mut diag = json!({});
    let parent = ctx.parents.get(&r.parent_id).map(String::as_str);
    let raw_content: Option<String> = if r.endpoint == "completion" {
        parsed["content"].as_str().map(str::to_owned)
    } else {
        parsed["choices"][0]["message"]["content"].as_str().map(str::to_owned)
    };
    if status != 200 {
        reasons.push(format!("http_status_{status}"));
    }
    let finish = if r.endpoint == "completion" {
        parsed["stop_type"].as_str().or(parsed["stopping_word"].as_str()).map(str::to_owned)
    } else {
        parsed["choices"][0]["finish_reason"].as_str().map(str::to_owned)
    };
    diag["finish_reason"] = json!(finish);
    let mut clean = String::new();
    match raw_content {
        None => reasons.push("no_content".into()),
        Some(c) => {
            let stripped = if r.endpoint == "completion" { Ok(c.clone()) } else { text::strip_empty_think(&c) };
            match stripped {
                Err(e) => reasons.push(e.into()),
                Ok(t) => {
                    if r.endpoint == "completion" {
                        let budget = r.cut_budget.unwrap_or(r.target_words);
                        diag["raw_words"] = json!(text::words(&t));
                        match text::cut_to_budget(&t, budget) {
                            Some(cut) => clean = cut,
                            None => reasons.push("no_sentence_end_within_budget".into()),
                        }
                    } else {
                        clean = t;
                    }
                }
            }
        }
    }
    // Post-processing (when configured) replaces the text that every check
    // below reads; its own rejection reasons join the others. Only profile
    // v2's step keeps the text it read as `raw_text`.
    let mut post_reasons = Vec::new();
    let mut raw_text: Option<String> = None;
    if reasons.is_empty()
        && let Some(post) = &ctx.post
    {
        let o = post.apply(r, parent, &clean);
        diag["post"] = o.record;
        post_reasons = o.reasons;
        let read = std::mem::replace(&mut clean, o.text);
        if post.is_profile_v2() {
            raw_text = Some(read);
        }
    }
    let mut out = json!({"accepted":false});
    if reasons.is_empty() {
        if clean.trim().is_empty() {
            reasons.push("empty_content".into());
        }
        let sp = text::special_tokens(&clean);
        if !sp.is_empty() {
            reasons.push("chat_template_special_tokens".into());
            diag["special_markers"] = json!(sp);
        }
        let w = text::words(&clean);
        diag["words"] = json!(w);
        let lo = (r.target_words as f64) * 0.5;
        let hi = (r.target_words as f64) * 1.5;
        if (w as f64) < lo || (w as f64) > hi {
            reasons.push("word_count_outside_0.5_1.5_of_target".into());
        }
        if r.template_id == crate::plan::BASE_CONT_TEMPLATE_ID && w < 20 {
            reasons.push("continuation_below_20_words".into());
        }
        if let (Some((s, e)), Some(p)) = (r.source_span, parent) {
            if clean == p[s..e] {
                reasons.push("byte_identical_to_replaced_span".into());
            }
            let unseen_run = matches!(r.op.as_deref(), Some("continue" | "prefix_continue" | "middle_infill"));
            if r.op.is_some() && r.endpoint == "chat" && !unseen_run {
                diag["span_word_change_fraction"] = json!(crate::diverse::change_fraction(&p[s..e], &clean));
            }
            if let Some(op) = r.op.as_deref() {
                reasons.extend(crate::span_ops::screen(op, &clean, p, (s, e), &mut diag));
            }
            // Train-side screen only: the protocol's Test robustness acceptance
            // has no copy screen beyond the general checks.
            if r.split != "test" && (r.endpoint == "completion" || r.op.as_deref() == Some("continue")) && overlap::shares_window(&clean, &p[s..e], 12) {
                // Train base cohort only: memorised continuation of the removed human text.
                reasons.push("copies_removed_human_text_12_word_window".into());
            }
            diag["shares_parent_50_word_window"] = json!(overlap::shares_window(&clean, p, 50));
        }
        // Span requests may overlap their own parent; on Test this covers the
        // robustness continuations and splices too (the parent is a dedup extra).
        let exclude_parent = r.source_span.is_some() && (r.endpoint == "chat" || r.split == "test");
        let conflicts = ctx.overlap.conflicts(&clean, if exclude_parent { parent } else { None });
        if !conflicts.is_empty() {
            reasons.push("duplicate_or_prior_overlap".into());
            diag["overlap"] = json!(conflicts);
        }
        if let Some(p) = parent {
            let doc = match r.source_span {
                Some(span) => assemble(p, &[span], &[clean.as_str()]).map(|x| x.0),
                None => Ok(clean.clone()),
            };
            if let (Ok(d), Some(win)) = (&doc, ctx.doc_words) {
                let n = text::words(d);
                diag["final_document_words"] = json!(n);
                reasons.extend(doc_window_reasons(n, win));
            }
            match doc {
                Ok(d) => match a_tokens(&ctx.tokenizer, &d) {
                    Ok(n) => {
                        diag["a_tokens_single_span_assembly"] = json!(n);
                        if n > crate::generation::MAX_A_TOKENS {
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
        reasons.extend(post_reasons);
        out["text"] = json!(clean);
        if let Some(t) = raw_text {
            out["raw_text"] = json!(t);
        }
    }
    out["accepted"] = json!(reasons.is_empty());
    out["reasons"] = json!(reasons);
    out["diag"] = diag;
    out
}

pub struct RunStats {
    pub processed: AtomicUsize,
    pub accepted: AtomicUsize,
    pub failed: AtomicUsize,
}

/// Acceptance function of one response: (context, request, HTTP status,
/// parsed body) -> verdict object with `accepted`, `reasons`, `diag` and, when
/// accepted, `text`.
pub type VerdictFn = dyn Fn(&Ctx, &Request, u16, &Value) -> Value + Send + Sync;

/// Process pending requests with `workers` concurrent slots.
pub fn run(ctx: Arc<Ctx>, pending: Vec<(Request, u32)>, out: &Path, workers: usize) -> Result<Value> {
    run_with(ctx, pending, out, workers, Arc::new(verdict))
}

/// As [`run`], with a caller-supplied acceptance function (spliced rewrites).
pub fn run_with(ctx: Arc<Ctx>, pending: Vec<(Request, u32)>, out: &Path, workers: usize, vf: Arc<VerdictFn>) -> Result<Value> {
    let queue = Arc::new(Mutex::new(pending.into_iter().collect::<std::collections::VecDeque<_>>()));
    let stats = Arc::new(RunStats { processed: AtomicUsize::new(0), accepted: AtomicUsize::new(0), failed: AtomicUsize::new(0) });
    let started = Instant::now();
    let t0 = now_ms();
    let mut handles = Vec::new();
    for w in 0..workers {
        let ctx = ctx.clone();
        let queue = queue.clone();
        let stats = stats.clone();
        let vf = vf.clone();
        let path = out.join(format!("journal-{t0}-w{w}.jsonl"));
        handles.push(std::thread::spawn(move || -> Result<()> {
            let mut j = Journal::open(&path)?;
            loop {
                let next = queue.lock().unwrap().pop_front();
                let Some((r, first)) = next else { break };
                let mut ok = false;
                // the request's limit (`Request::attempts`): ATTEMPTS unless
                // it carries a regenerated mirror's remaining attempts
                for attempt in first..r.attempts() {
                    let seed = r.seed.wrapping_add(attempt);
                    let b = body(&ctx, &r, seed);
                    let mut intent = json!({"kind":"intent","request_id":r.request_id,"item_id":r.item_id,"split":r.split,
                        "generator":r.generator,"attempt":attempt,"seed":seed,"t":now_ms(),"body":b});
                    // a carried limit is journaled, so journal readers see it
                    // (`ReqState::done`); R0's intent bytes are unchanged
                    if let Some(l) = r.attempt_limit {
                        intent["attempt_limit"] = json!(l);
                    }
                    j.write(&intent)?;
                    let url = if r.endpoint == "completion" { format!("{}/completion", ctx.endpoint) } else { format!("{}/v1/chat/completions", ctx.endpoint) };
                    let ts = now_ms();
                    let clock = Instant::now();
                    let (status, raw, parsed, err) = match ctx.client.post(&url).json(&b).send() {
                        Ok(resp) => {
                            let st = resp.status().as_u16();
                            match resp.text() {
                                Ok(t) => {
                                    let p = serde_json::from_str::<Value>(&t).unwrap_or(Value::Null);
                                    (st, t, p, None)
                                }
                                Err(e) => (st, String::new(), Value::Null, Some(e.to_string())),
                            }
                        }
                        Err(e) => (0, String::new(), Value::Null, Some(e.to_string())),
                    };
                    let v = if err.is_some() {
                        json!({"accepted":false,"reasons":["transport_error"],"diag":{"error":err}})
                    } else {
                        vf(&ctx, &r, status, &parsed)
                    };
                    let accepted = v["accepted"] == true;
                    if accepted {
                        ctx.overlap.add_generated(v["text"].as_str().unwrap());
                    }
                    j.write(&json!({"kind":"response","request_id":r.request_id,"item_id":r.item_id,"split":r.split,
                        "generator":r.generator,"attempt":attempt,"seed":seed,"t_start":ts,"t_end":now_ms(),
                        "elapsed_ms":clock.elapsed().as_millis(),"status":status,"raw_body":raw,"timings":parsed["timings"],
                        "usage":parsed["usage"],"verdict":v}))?;
                    if accepted {
                        ok = true;
                        break;
                    }
                    if err.is_some() {
                        // Server unreachable or the body was lost: stop this worker; the
                        // attempt stays consumed and a restart resumes the rest.
                        anyhow::bail!("transport error on {}: {:?}", r.request_id, err);
                    }
                }
                stats.processed.fetch_add(1, Ordering::Relaxed);
                if ok { stats.accepted.fetch_add(1, Ordering::Relaxed) } else { stats.failed.fetch_add(1, Ordering::Relaxed) };
                let p = stats.processed.load(Ordering::Relaxed);
                if p % 50 == 0 {
                    eprintln!("{} processed {} accepted {} failed {} in {:.1}s", r.generator, p, stats.accepted.load(Ordering::Relaxed),
                        stats.failed.load(Ordering::Relaxed), started.elapsed().as_secs_f64());
                }
            }
            Ok(())
        }));
    }
    for h in handles {
        h.join().map_err(|_| anyhow::anyhow!("worker panicked"))??;
    }
    let secs = started.elapsed().as_secs_f64();
    let p = stats.processed.load(Ordering::Relaxed);
    Ok(json!({"processed":p,"accepted":stats.accepted.load(Ordering::Relaxed),"failed":stats.failed.load(Ordering::Relaxed),
        "seconds":secs,"records_per_minute": if secs > 0.0 { p as f64 * 60.0 / secs } else { 0.0 },"workers":workers}))
}

pub fn client() -> Result<reqwest::blocking::Client> {
    Ok(reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(900))
        .redirect(reqwest::redirect::Policy::none())
        .build()?)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn final_document_window_is_inclusive() {
        assert!(doc_window_reasons(150, (150, 450)).is_empty());
        assert!(doc_window_reasons(450, (150, 450)).is_empty());
        assert_eq!(doc_window_reasons(149, (150, 450)), vec!["final_document_below_150_words".to_string()]);
        assert_eq!(doc_window_reasons(451, (150, 450)), vec!["final_document_above_450_words".to_string()]);
    }

    #[test]
    fn assemble_two_spans_exact_accounting() {
        let src = "One two. Three four five. Six seven. Eight.";
        let s = crate::text::sentences(src);
        let spans = [(s[0].start, s[0].end), (s[2].start, s[3].end)];
        let (t, fin, share) = assemble(src, &spans, &["Uno dos tres.", "Seis."]).unwrap();
        assert_eq!(t, "Uno dos tres. Three four five. Seis.");
        assert_eq!(&t[fin[0].0..fin[0].1], "Uno dos tres.");
        assert_eq!(&t[fin[1].0..fin[1].1], "Seis.");
        assert!((share - 4.0 / 7.0).abs() < 1e-12);
        let orig: Vec<&str> = spans.iter().map(|&(a, b)| &src[a..b]).collect();
        assert_eq!(assemble(src, &spans, &orig).unwrap().0, src);
        assert!(assemble(src, &[(1, 5)], &["x"]).is_err());
        assert!(assemble(src, &spans[..1], &[" padded"]).is_err());
    }

    /// A post step that rewrites the text and states whether it is profile
    /// v2's (B1c: `raw_text` follows that statement alone).
    struct Step {
        v2: bool,
    }

    impl PostProcess for Step {
        fn apply(&self, _r: &Request, _parent: Option<&str>, clean: &str) -> PostOut {
            PostOut { text: clean.replace("ferry", "barge"), reasons: Vec::new(), record: json!({"step": "synthetic", "v2": self.v2}) }
        }
        fn is_profile_v2(&self) -> bool {
            self.v2
        }
    }

    fn step_ctx(post: Option<Arc<dyn PostProcess>>, parent: &str) -> Ctx {
        Ctx {
            endpoint: String::new(),
            alias: "m".into(),
            config: json!({}),
            tokenizer: r#"{"version":"1.0","truncation":null,"padding":null,"added_tokens":[],"normalizer":null,"pre_tokenizer":{"type":"Whitespace"},"post_processor":null,"decoder":null,"model":{"type":"WordLevel","vocab":{"[UNK]":0},"unk_token":"[UNK]"}}"#
                .parse()
                .unwrap(),
            overlap: Overlap::new(overlap::PriorIndex::empty()),
            parents: [("p".to_string(), parent.to_string())].into(),
            client: client().unwrap(),
            post,
            doc_words: None,
        }
    }

    #[test]
    fn only_a_profile_v2_step_keeps_raw_text() {
        // synthetic parent and mirror reply (no corpus text)
        let parent = "The harbour lamps were lit before the ferry came in. The quay stayed quiet until the last boat was tied up for the night.";
        let reply = "The pier lights came on before the ferry docked. Nobody on the quay spoke until the final boat was secured for the evening.";
        let r: Request = serde_json::from_value(json!({"request_id": "r1", "item_id": "i1", "parent_id": "p", "split": "train", "generator": "g",
            "endpoint": "chat", "template_id": "t", "prompt": "p", "span_index": 0, "source_span": null, "target_words": 23, "cut_budget": null,
            "decoding": {"profile": "A", "temperature": 0.7, "top_p": 0.9, "top_k": 0, "min_p": 0.0, "repeat_penalty": 1.0, "max_tokens": 512},
            "request_sha256": "x", "seed": 1}))
        .unwrap();
        let parsed = json!({"choices":[{"message":{"content":reply},"finish_reason":"stop"}]});
        let off = verdict(&step_ctx(None, parent), &r, 200, &parsed);
        let v1 = verdict(&step_ctx(Some(Arc::new(Step { v2: false })), parent), &r, 200, &parsed);
        let v2 = verdict(&step_ctx(Some(Arc::new(Step { v2: true })), parent), &r, 200, &parsed);
        for v in [&off, &v1, &v2] {
            assert_eq!(v["accepted"], true, "{v}");
        }
        // profile off: no post record and no raw_text
        assert!(off["diag"].get("post").is_none() && off.get("raw_text").is_none(), "{off}");
        assert_eq!(off["text"].as_str(), Some(reply));
        // a step that is not profile v2's: its record and its text, no raw_text
        assert!(v1.get("raw_text").is_none(), "{v1}");
        assert_eq!(v1["diag"]["post"]["step"], "synthetic");
        assert_eq!(v1["text"].as_str(), Some(reply.replace("ferry", "barge").as_str()));
        // profile v2's step: the same verdict plus the cleaned response it read
        assert_eq!(v2["raw_text"].as_str(), Some(reply));
        let mut v2_less = v2.clone();
        v2_less.as_object_mut().unwrap().remove("raw_text");
        v2_less["diag"]["post"]["v2"] = json!(false);
        assert_eq!(v2_less, v1);
        // a verdict the standard checks reject keeps its text; raw_text again
        // only under profile v2's step
        let short = json!({"choices":[{"message":{"content":"The ferry left early."},"finish_reason":"stop"}]});
        let s1 = verdict(&step_ctx(Some(Arc::new(Step { v2: false })), parent), &r, 200, &short);
        let s2 = verdict(&step_ctx(Some(Arc::new(Step { v2: true })), parent), &r, 200, &short);
        assert_eq!(s1["accepted"], false);
        assert_eq!(s1["text"].as_str(), Some("The barge left early."));
        assert!(s1.get("raw_text").is_none(), "{s1}");
        assert_eq!(s2["raw_text"].as_str(), Some("The ferry left early."));
        // a response rejected before the step runs no step at all
        let empty = json!({"choices":[{"message":{"content":null},"finish_reason":"stop"}]});
        let n2 = verdict(&step_ctx(Some(Arc::new(Step { v2: true })), parent), &r, 200, &empty);
        assert!(n2["diag"].get("post").is_none() && n2.get("raw_text").is_none(), "{n2}");
    }

    #[test]
    fn uncertain_intent_consumes_an_attempt() {
        let mut st = ReqState::default();
        st.intents.push(0);
        assert_eq!(st.used(), 1);
        assert!(!st.done());
        st.intents.push(1);
        assert!(st.done());
    }

    #[test]
    fn a_request_limit_bounds_its_attempts() {
        let r: Request = serde_json::from_value(json!({"request_id": "r1", "item_id": "i1", "parent_id": "p", "split": "train", "generator": "g",
            "endpoint": "chat", "template_id": "t", "prompt": "p", "span_index": 0, "source_span": null, "target_words": 23, "cut_budget": null,
            "decoding": {"profile": "A", "temperature": 0.7, "top_p": 0.9, "top_k": 0, "min_p": 0.0, "repeat_penalty": 1.0, "max_tokens": 512},
            "request_sha256": "x", "seed": 1}))
        .unwrap();
        // absent: R0's two, and the field is not serialised (request bytes unchanged)
        assert_eq!((r.attempt_limit, r.attempts()), (None, ATTEMPTS));
        assert!(!serde_json::to_string(&r).unwrap().contains("attempt_limit"));
        let mut st = ReqState::default();
        assert!(st.done_within(0) && !st.done_within(1) && !st.done());
        st.intents.push(0);
        assert!(st.done_within(1) && !st.done_within(2) && !st.done());
        let one = Request { attempt_limit: Some(1), ..r.clone() };
        assert_eq!(one.attempts(), 1);
        assert!(serde_json::to_string(&one).unwrap().contains("\"attempt_limit\":1"));
        let back: Request = serde_json::from_str(&serde_json::to_string(&one).unwrap()).unwrap();
        assert_eq!(back.attempts(), 1);
    }

    #[test]
    fn a_journaled_limit_bounds_done_for_readers_without_the_request() {
        // B5b review finding 6: rates and stratify-train hold journals only
        let dir = tempfile::tempdir().unwrap();
        let j = dir.path().join("journal-1-w0.jsonl");
        let lines = [
            json!({"kind": "intent", "request_id": "carried", "split": "train", "attempt": 0, "attempt_limit": 1}),
            json!({"kind": "response", "request_id": "carried", "split": "train", "attempt": 0, "verdict": {"accepted": false}}),
            json!({"kind": "intent", "request_id": "r0", "split": "train", "attempt": 0}),
            json!({"kind": "response", "request_id": "r0", "split": "train", "attempt": 0, "verdict": {"accepted": false}}),
        ];
        std::fs::write(&j, lines.iter().map(|l| format!("{l}\n")).collect::<String>()).unwrap();
        let ids: std::collections::HashSet<String> = ["carried".to_string(), "r0".to_string()].into();
        for st in [load_states(&[dir.path().to_owned()]).unwrap(), load_states_of_split(&[dir.path().to_owned()], "train", &ids).unwrap().0] {
            assert_eq!(st["carried"].limit, Some(1));
            assert!(st["carried"].done() && st["carried"].done_within(1));
            assert_eq!(st["r0"].limit, None);
            assert!(!st["r0"].done() && st["r0"].done_within(1));
        }
        // intents of one request carrying different limits are refused
        let more = json!({"kind": "intent", "request_id": "carried", "split": "train", "attempt": 1, "attempt_limit": 2});
        std::fs::write(&j, format!("{}\n{more}\n", lines[0])).unwrap();
        assert!(load_states(&[dir.path().to_owned()]).unwrap_err().to_string().contains("different attempt limits"));
    }
}
