//! Spliced-rewrite span data (A span protocol spans-v1, Section 3).
//!
//! A human-only parent keeps most of its text. Selected sentences, clauses,
//! paragraphs or runs are rewritten by a local open-weight generator, or new
//! sentences are inserted, and the outputs are spliced back in. Every record
//! carries the exact model-produced byte and character spans of its final
//! text, the realised final-word share and the complete edit history. The
//! untouched parent is the paired human negative (same split and source
//! group). These rows are source-conditioned mixed rewrites, never writing
//! generated from scratch.
//!
//! Selection policies, operations, generators, voices and decoding are keyed
//! by SHA-256 of the item identity, so a plan is reproducible from its inputs.
//! All edits of one document are requested against the original parent,
//! independently. Words and span boundaries follow the Python `str.isspace`
//! rule that the runtime and trainer use.
use crate::diverse::{self, Brief};
use crate::generation::{Ctx, ReqState};
use crate::overlap::Overlap;
use crate::plan::{self, Decoding, Request};
use crate::text::{self, Sentence};
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::BufRead;
use std::path::{Path, PathBuf};

pub const KEY_PREFIX: &str = "slopninja-a-spans-v1:";
pub const PLAN_SCHEMA: &str = "slop_ninja_a_spliced_plan_v1";
pub const RECORD_SCHEMA: &str = "slop_ninja_a_spliced_rewrite_v1";
pub const FAILURE_SCHEMA: &str = "slop_ninja_a_spliced_failure_v1";
pub const COHORT_ID: &str = "spliced_rewrite_v1";
pub const SOURCE_KIND: &str = "source_conditioned_mixed_rewrite";
pub const CODE_ID: &str = "grammar-a-positives-splice-v1";
pub const WORD_POLICY: &str = "python_str_isspace_runs_v1";
pub const MAX_PARENT_TOKENS: usize = 1800;
pub const MAX_DOC_TOKENS: usize = 2048;
pub const TRAIN: &str = "train";
pub const DEV_SET: &str = "dev_iter_span_eval";
pub const RAID_DOMAINS: [&str; 3] = ["raid_books", "raid_reddit", "raid_wiki"];
pub const HELD_DOMAINS: [&str; 1] = ["ellipse"];
/// Seven Dev-iter families (no gpt-oss, no base checkpoints).
pub const DEV_GENERATORS: [&str; 7] = plan::FAMILY_ORDER;

// ---------------------------------------------------------------- keys

pub fn key(x: &str, id: &str) -> String {
    text::sha_hex(format!("{KEY_PREFIX}{x}:{id}").as_bytes())
}
pub fn ukey(x: &str, id: &str) -> u64 {
    u64::from_str_radix(&key(x, id)[..15], 16).unwrap()
}

// ---------------------------------------------------------------- words

/// Python `str.isspace`: Unicode White_Space plus U+001C..U+001F.
pub fn py_space(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}
/// Byte ranges of maximal non-whitespace runs (the share words).
pub fn py_word_ranges(s: &str) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut start: Option<usize> = None;
    for (i, c) in s.char_indices() {
        if py_space(c) {
            if let Some(a) = start.take() {
                out.push((a, i));
            }
        } else if start.is_none() {
            start = Some(i);
        }
    }
    if let Some(a) = start {
        out.push((a, s.len()));
    }
    out
}
pub fn py_words(s: &str) -> usize {
    py_word_ranges(s).len()
}

// ---------------------------------------------------------------- parents

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Parent {
    pub id: String,
    /// `train` or `dev_iter`
    pub split: String,
    pub domain: String,
    pub genre: Option<String>,
    pub source_group: String,
    pub writer_key: Option<String>,
    pub cohort_id: String,
    pub provenance_sha256: Option<String>,
    pub text: String,
    pub text_sha256: String,
    pub a_tokens: usize,
    pub words: usize,
}

/// Sentence and paragraph structure of one parent (sealed splitter).
pub struct Doc<'a> {
    pub text: &'a str,
    pub sents: Vec<Sentence>,
    pub words: Vec<usize>,
    /// paragraph index of every sentence (blank-line breaks)
    pub para: Vec<usize>,
    pub n_paras: usize,
}

impl<'a> Doc<'a> {
    pub fn new(text: &'a str) -> Self {
        let sents = text::sentences(text);
        let words = sents.iter().map(|s| py_words(&text[s.start..s.end])).collect();
        let mut para = Vec::with_capacity(sents.len());
        let mut p = 0;
        for i in 0..sents.len() {
            if i > 0 && text[sents[i - 1].end..sents[i].start].matches('\n').count() >= 2 {
                p += 1;
            }
            para.push(p);
        }
        let n_paras = if sents.is_empty() { 0 } else { p + 1 };
        Doc { text, sents, words, para, n_paras }
    }
    pub fn n(&self) -> usize {
        self.sents.len()
    }
    pub fn span(&self, a: usize, b: usize) -> (usize, usize) {
        (self.sents[a].start, self.sents[b - 1].end)
    }
    pub fn sent(&self, i: usize) -> &'a str {
        &self.text[self.sents[i].start..self.sents[i].end]
    }
    pub fn run_words(&self, a: usize, b: usize) -> usize {
        self.words[a..b].iter().sum()
    }
    fn rewritable(&self, i: usize) -> bool {
        (4..=120).contains(&self.words[i]) && self.sent(i).chars().any(char::is_alphabetic)
    }
}

// ---------------------------------------------------------------- selection

/// One edit unit chosen by a selection policy.
#[derive(Clone, Debug, PartialEq)]
pub enum Unit {
    /// whole-sentence run [a, b)
    Run { a: usize, b: usize },
    /// 2-5 adjacent sentences replaced by one shorter passage
    Condense { a: usize, b: usize },
    /// sub-sentence clause: byte span inside sentence `sent`
    Clause { sent: usize, s: usize, e: usize },
    /// new text inserted right after sentence `after`
    Insert { after: usize, at_end: bool },
}

pub const POLICIES: [(&str, usize); 8] = [
    ("one_sentence", 7),
    ("scattered", 9),
    ("paragraph", 7),
    ("most", 4),
    ("clause", 8),
    ("insert", 6),
    ("condense", 5),
    ("compound", 4),
];
pub const COMPOUND_PARTS: [&str; 6] = ["one_sentence", "scattered", "paragraph", "clause", "insert", "condense"];

/// The 50-slot policy cycle, interleaved so consecutive slots spread policies
/// (slot positions (k + 0.5) / count, ties by policy order).
pub fn policy_cycle() -> Vec<&'static str> {
    let mut v: Vec<(f64, usize, &str)> = Vec::new();
    for (pi, (p, c)) in POLICIES.iter().enumerate() {
        for k in 0..*c {
            v.push(((k as f64 + 0.5) / *c as f64, pi, p));
        }
    }
    v.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
    v.into_iter().map(|x| x.2).collect()
}

fn keyed_order(idx: impl Iterator<Item = usize>, tag: &str, id: &str) -> Vec<usize> {
    let mut v: Vec<(String, usize)> = idx.map(|i| (key(tag, &format!("{id}:{i}")), i)).collect();
    v.sort();
    v.into_iter().map(|x| x.1).collect()
}

fn is_clause_punct(c: char) -> bool {
    matches!(c, ',' | ';' | ':' | '—' | '–')
}
fn is_terminal(c: char) -> bool {
    matches!(c, '.' | '!' | '?' | '…')
}
fn is_closer(c: char) -> bool {
    matches!(c, '"' | '\'' | ')' | ']' | '”' | '’' | '»' | '}')
}
fn is_opener(c: char) -> bool {
    matches!(c, '"' | '\'' | '(' | '[' | '“' | '‘' | '«' | '{')
}

/// Clause candidates of one sentence: maximal word runs between clause
/// punctuation (`, ; : — –` ending a word, or a standalone dash word), 3-25
/// words, never the whole sentence, never across a line break.
pub fn clause_segments(doc: &Doc, i: usize) -> Vec<(usize, usize)> {
    let s = doc.sents[i];
    let ws: Vec<(usize, usize)> = py_word_ranges(&doc.text[s.start..s.end]).into_iter().map(|(a, b)| (a + s.start, b + s.start)).collect();
    let mut segs: Vec<(usize, usize)> = Vec::new();
    let mut cur: Option<usize> = None;
    for (k, &(a, b)) in ws.iter().enumerate() {
        let w = &doc.text[a..b];
        if w.chars().all(|c| matches!(c, '—' | '–' | '-')) {
            if let Some(st) = cur.take() {
                segs.push((st, k));
            }
            continue;
        }
        if cur.is_none() {
            cur = Some(k);
        }
        if w.chars().last().is_some_and(is_clause_punct) && k + 1 < ws.len() {
            segs.push((cur.take().unwrap(), k + 1));
        }
    }
    if let Some(st) = cur {
        segs.push((st, ws.len()));
    }
    segs.into_iter()
        .filter(|&(a, b)| (3..=25).contains(&(b - a)) && !(a == 0 && b == ws.len()))
        .map(|(a, b)| (ws[a].0, ws[b - 1].1))
        .filter(|&(x, y)| !doc.text[x..y].contains(['\n', '\r']) && doc.text[x..y].chars().any(char::is_alphabetic))
        .collect()
}

/// Edit units of `policy` restricted to sentences [lo, hi). `whole` marks the
/// whole-document call (some human text must remain); `allow_end` permits an
/// insertion after the last sentence.
pub fn select(policy: &str, doc: &Doc, id: &str, lo: usize, hi: usize, whole: bool, allow_end: bool) -> Option<Vec<Unit>> {
    let n = doc.n();
    if n == 0 || lo >= hi {
        return None;
    }
    let u = |x: &str| ukey(&format!("sel:{policy}:{x}"), id);
    match policy {
        "one_sentence" => {
            if whole && n < 2 {
                return None;
            }
            let c: Vec<usize> = (lo..hi).filter(|&i| doc.rewritable(i)).collect();
            if c.is_empty() {
                return None;
            }
            let i = c[(u("pick") % c.len() as u64) as usize];
            Some(vec![Unit::Run { a: i, b: i + 1 }])
        }
        "scattered" => {
            let k = 2 + (u("k") % 4) as usize;
            let order = keyed_order((lo..hi).filter(|&i| doc.rewritable(i)), "scattered", id);
            let mut pick: Vec<usize> = Vec::new();
            for i in order {
                if pick.len() == k {
                    break;
                }
                if pick.iter().all(|&j| i.abs_diff(j) >= 2) {
                    pick.push(i);
                }
            }
            if pick.len() < 2 {
                return None;
            }
            pick.sort();
            Some(pick.into_iter().map(|i| Unit::Run { a: i, b: i + 1 }).collect())
        }
        "paragraph" => {
            if doc.n_paras >= 2 {
                let mut paras: Vec<(usize, usize)> = Vec::new();
                let mut a = 0;
                for i in 1..=n {
                    if i == n || doc.para[i] != doc.para[i - 1] {
                        paras.push((a, i));
                        a = i;
                    }
                }
                let c: Vec<(usize, usize)> = paras
                    .into_iter()
                    .filter(|&(a, b)| a >= lo && b <= hi && b - a >= 2 && (8..=900).contains(&doc.run_words(a, b)) && !(whole && a == 0 && b == n))
                    .collect();
                if !c.is_empty() {
                    let (a, b) = c[(u("para") % c.len() as u64) as usize];
                    return Some(vec![Unit::Run { a, b }]);
                }
            }
            let avail = (hi - lo).saturating_sub(if whole { 1 } else { 0 });
            let l = (3 + (u("len") % 6) as usize).min(avail);
            if l < 3 {
                return None;
            }
            let span = hi - lo - l + 1;
            let a = lo + (u("start") % span as u64) as usize;
            let (a, b) = if whole && a == 0 && a + l == n { (1, 1 + l) } else { (a, a + l) };
            Some(vec![Unit::Run { a, b }])
        }
        "most" => {
            if !whole || n < 4 {
                return None;
            }
            let c = (1 + (u("keep") % 3) as usize).min(n - 3);
            match u("pos") % 3 {
                0 => Some(vec![Unit::Run { a: c, b: n }]),
                1 => {
                    let m = (n - c) / 2;
                    Some(vec![Unit::Run { a: 0, b: m }, Unit::Run { a: m + c, b: n }])
                }
                _ => Some(vec![Unit::Run { a: 0, b: n - c }]),
            }
        }
        "clause" => {
            let k = 1 + (u("k") % 3) as usize;
            let order = keyed_order((lo..hi).filter(|&i| doc.words[i] >= 6), "clause_sent", id);
            let mut out = Vec::new();
            for i in order {
                if out.len() == k {
                    break;
                }
                let segs = clause_segments(doc, i);
                if segs.is_empty() {
                    continue;
                }
                let (s, e) = segs[(ukey("clause_pick", &format!("{id}:{i}")) % segs.len() as u64) as usize];
                out.push(Unit::Clause { sent: i, s, e });
            }
            if out.is_empty() {
                return None;
            }
            out.sort_by_key(|x| match x {
                Unit::Clause { s, .. } => *s,
                _ => 0,
            });
            Some(out)
        }
        "insert" => {
            let k = 1 + (u("k") % 3) as usize;
            let gaps = (lo..hi).filter(|&i| doc.sents[i].punctuated && (i + 1 < n || allow_end) && doc.words[i] >= 3);
            let order = keyed_order(gaps, "insert_gap", id);
            let mut pick: Vec<usize> = Vec::new();
            for i in order {
                if pick.len() == k {
                    break;
                }
                if pick.iter().all(|&j| i.abs_diff(j) >= 2) {
                    pick.push(i);
                }
            }
            if pick.is_empty() {
                return None;
            }
            pick.sort();
            Some(pick.into_iter().map(|i| Unit::Insert { after: i, at_end: i + 1 == n }).collect())
        }
        "condense" => {
            let avail = (hi - lo).saturating_sub(if whole { 1 } else { 0 });
            let l = (2 + (u("len") % 4) as usize).min(avail);
            if l < 2 {
                return None;
            }
            let starts = keyed_order(lo..=(hi - l), "condense_start", id);
            for a in starts {
                let b = a + l;
                if whole && a == 0 && b == n {
                    continue;
                }
                if doc.run_words(a, b) >= 12 && doc.run_words(a, b) <= 400 {
                    return Some(vec![Unit::Condense { a, b }]);
                }
            }
            None
        }
        _ => None,
    }
}

/// Policy selection with the compound policy. Returns (component, unit) pairs.
pub fn select_policy(policy: &str, doc: &Doc, id: &str) -> Option<Vec<(String, Unit)>> {
    let n = doc.n();
    if policy != "compound" {
        return select(policy, doc, id, 0, n, true, true).map(|v| v.into_iter().map(|x| (policy.to_owned(), x)).collect());
    }
    if n < 6 {
        return None;
    }
    let u = ukey("compound", id);
    let a = (u % 6) as usize;
    let b = (a + 1 + ((u / 6) % 5) as usize) % 6;
    let (pa, pb) = if (u / 30) % 2 == 0 { (COMPOUND_PARTS[a], COMPOUND_PARTS[b]) } else { (COMPOUND_PARTS[b], COMPOUND_PARTS[a]) };
    let m = n / 2;
    let x = select(pa, doc, &format!("{id}:cA"), 0, m, false, false)?;
    let y = select(pb, doc, &format!("{id}:cB"), m + 1, n, false, true)?;
    let mut out: Vec<(String, Unit)> = x.into_iter().map(|u| (pa.to_owned(), u)).collect();
    out.extend(y.into_iter().map(|u| (pb.to_owned(), u)));
    Some(out)
}

// ---------------------------------------------------------------- operations

/// Whole-sentence / run operation slots (44): family weights paraphrase 18,
/// style 22, polish 16, expand 10, condense 10, humanize 12 (of 88).
pub const RUN_SLOTS: [(&str, usize); 11] = [
    ("paraphrase", 9),
    ("voiced_paraphrase", 3),
    ("casual", 3),
    ("formalize", 3),
    ("simplify", 2),
    ("polish", 4),
    ("spelling", 2),
    ("fix_slop", 2),
    ("expand", 5),
    ("condense", 5),
    ("humanize", 6),
];
/// Clause operation slots (40).
pub const CLAUSE_SLOTS: [(&str, usize); 7] =
    [("paraphrase", 10), ("formalize", 4), ("casual", 4), ("simplify", 3), ("polish", 8), ("humanize", 6), ("expand", 5)];

pub fn family_of(op: &str) -> &'static str {
    match op {
        "paraphrase" => "paraphrase",
        "voiced_paraphrase" | "casual" | "formalize" | "simplify" => "style",
        "polish" | "spelling" | "fix_slop" => "polish",
        "expand" => "expand",
        "condense" => "condense",
        "humanize" => "humanize",
        "insert_bridge" | "insert_elaborate" | "continue" => "continuation",
        _ => "other",
    }
}

/// Operations that can carry a voice or brief. Plain `paraphrase` is the
/// unvoiced paraphrase family; `voiced_paraphrase` always carries one.
pub fn style_capable(op: &str) -> bool {
    matches!(op, "voiced_paraphrase" | "expand" | "condense" | "casual" | "humanize" | "simplify" | "insert_bridge" | "insert_elaborate" | "continue")
}

fn slot_pick(slots: &[(&'static str, usize)], u: u64) -> &'static str {
    let total: usize = slots.iter().map(|x| x.1).sum();
    let mut r = (u % total as u64) as usize;
    for (op, c) in slots {
        if r < *c {
            return op;
        }
        r -= c;
    }
    unreachable!()
}

pub fn min_change(op: &str) -> f64 {
    if matches!(op, "polish" | "spelling" | "fix_slop") { 0.02 } else { 0.10 }
}

// ---------------------------------------------------------------- plan types

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Edit {
    pub edit_index: usize,
    /// policy component (the policy itself, or a compound part)
    pub component: String,
    /// run | condense | clause | insert
    pub kind: String,
    pub operation: String,
    pub family: String,
    /// keyed operation before plan-time feasibility fallback
    pub operation_requested: String,
    pub generator: String,
    /// [s, e) in the parent; s == e for an insertion
    pub parent_span: (usize, usize),
    /// separator inserted before an insertion (never part of a span)
    pub insert_sep: Option<String>,
    pub sentence_range: (usize, usize),
    pub context_included: bool,
    /// parent byte ranges shown to the generator as read-only context
    pub context_spans: Vec<(usize, usize)>,
    pub style: Value,
    pub template_id: String,
    pub source_words: usize,
    pub target_words: usize,
    /// accepted output word range [lo, hi]
    pub len_lo: f64,
    pub len_hi: f64,
    pub min_change: Option<f64>,
    pub start_rule: String,
    pub end_rule: String,
    pub newline_ok: bool,
    pub request_id: String,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Item {
    pub item_id: String,
    /// train | dev_iter_span_eval
    pub population: String,
    pub split: String,
    pub domain: String,
    pub parent_id: String,
    pub parent_text_sha256: String,
    pub source_group: String,
    pub writer_key: Option<String>,
    pub variant: usize,
    pub order: usize,
    /// stratified position (j + 0.5) / allocation; > 1 marks reserve items
    pub order_t: f64,
    pub policy: String,
    pub policy_requested: String,
    pub policy_fallback_steps: usize,
    pub generators: Vec<String>,
    pub two_generators: bool,
    /// document | edit
    pub voice_scope: String,
    pub span_half: Option<String>,
    pub edits: Vec<Edit>,
}

// ---------------------------------------------------------------- prompts

pub const T_RUN: &str = "a_spans_v1_run_rewrite_v3";
pub const T_RUN_CTX: &str = "a_spans_v1_run_rewrite_context_v3";
pub const T_CLAUSE: &str = "a_spans_v1_clause_rewrite_v3";
pub const T_BRIDGE: &str = "a_spans_v1_insert_bridge";
pub const T_ELABORATE: &str = "a_spans_v1_insert_elaborate";
pub const T_CONTINUE: &str = "a_spans_v1_insert_continue";

pub const CONTEXT_BLOCK: &str = "The passage comes from a longer text. For context only, here is the text just before it and just after it; do not rewrite, repeat or continue the context.\n\nBEFORE: {before}\nAFTER: {after}";
/// Shape and length of a run rewrite (added after the plan-v2 instruction; pilot amendment D1).
pub const SHAPE_TEMPLATE: &str = "Write your version as {shape} of about {n} words (no more than {cap}){nl}.";
pub const CLAUSE_TEMPLATE: &str = "Here is a sentence from a longer text{ctx_intro}:\n\n{sentence_block}\n\nRewrite only this part of the sentence:\n\n{clause}\n\n{task}{style} Return only the new wording for that part, in about {n} words, with no quotation marks, label, preface or explanation, and do not repeat the rest of the sentence. Your wording goes back into the sentence in place of that part, so it must {fit}.";
pub const BRIDGE_TEMPLATE: &str = "Here are two consecutive sentences from {genre}:\n\nFIRST: {first}\nSECOND: {second}\n\nWrite one new sentence of about {n} words to go between them, so that the text reads naturally from FIRST to your sentence to SECOND. {voice}Do not invent specific names, numbers, dates or quotations. Return only the new sentence, with no quotation marks, label, preface or explanation.";
pub const ELABORATE_TEMPLATE: &str = "Here is a sentence from {genre}{ctx_intro}:\n\n{sentence_block}\n\nWrite one new sentence of about {n} words to follow it, explaining or elaborating on what it says. {voice}Do not invent specific names, numbers, dates or quotations. Return only the new sentence, with no quotation marks, label, preface or explanation.";
pub const CONTINUE_TEMPLATE: &str = "Here is {genre}:\n\n{text}\n\nWrite one or two new sentences, about {n} words in total, that continue it naturally from where it ends. {voice}Do not repeat what has been said and do not invent specific names, numbers, dates or quotations. Return only the new sentences, with no quotation marks, label, preface or explanation.";

pub fn template_hashes() -> Value {
    json!({
        T_RUN: {"basis":"plan-v2 diverse::rewrite_prompt verbatim","sha256":text::sha_hex(include_bytes!("diverse.rs"))},
        T_RUN_CTX: {"basis":"plan-v2 diverse::rewrite_prompt with CONTEXT_BLOCK before the passage","sha256":text::sha_hex(CONTEXT_BLOCK.as_bytes())},
        "a_spans_v1_run_shape": {"basis":"appended to every run instruction","sha256":text::sha_hex(SHAPE_TEMPLATE.as_bytes())},
        T_CLAUSE: {"sha256":text::sha_hex(CLAUSE_TEMPLATE.as_bytes())},
        T_BRIDGE: {"sha256":text::sha_hex(BRIDGE_TEMPLATE.as_bytes())},
        T_ELABORATE: {"sha256":text::sha_hex(ELABORATE_TEMPLATE.as_bytes())},
        T_CONTINUE: {"sha256":text::sha_hex(CONTINUE_TEMPLATE.as_bytes())},
    })
}

/// plan-v2 prompt plus the shape sentence and optional read-only context.
fn compose_run(prompt: &str, span: &str, shape: &str, context: Option<(Option<&str>, Option<&str>)>) -> String {
    let head = prompt.strip_suffix(span).expect("plan-v2 prompt ends with the passage");
    let (instr, fix) = match head.strip_suffix("PASSAGE TO EDIT:\n") {
        Some(h) => (h.trim_end(), true),
        None => (head.trim_end(), false),
    };
    let mut out = if fix { format!("{instr}\n\n{shape}") } else { format!("{instr} {shape}") };
    match context {
        Some((before, after)) => {
            let block = CONTEXT_BLOCK.replace("{before}", before.unwrap_or("(start of text)")).replace("{after}", after.unwrap_or("(end of text)"));
            out.push_str(&format!("\n\n{block}\n\n{}\n", if fix { "PASSAGE TO EDIT:" } else { "PASSAGE:" }));
        }
        None => out.push_str(if fix { "\n\nPASSAGE TO EDIT:\n" } else { "\n\n" }),
    }
    out.push_str(span);
    out
}

/// Word cap stated in the run prompt: inside the acceptance bound with margin.
pub fn run_cap(op: &str, source_words: usize, target: usize) -> usize {
    let w = source_words as f64;
    let cap = match op {
        "expand" => (2.0 * w).floor(),
        "condense" => (0.75 * w).floor(),
        _ => (1.4 * w).floor(),
    } as usize;
    cap.max(target + 1)
}

pub fn shape_sentence(op: &str, k: usize, target: usize, cap: usize, newline_ok: bool, condense_policy: bool) -> String {
    let shape = match (k, op) {
        (1, "expand") => "one or two sentences".to_owned(),
        (1, _) => "a single sentence".to_owned(),
        (_, "expand") => "a longer passage".to_owned(),
        (_, "condense") => "a shorter passage".to_owned(),
        _ if condense_policy => "a shorter passage".to_owned(),
        (k, _) if k <= 8 => format!("{k} sentences"),
        (k, _) => format!("a passage of about {k} sentences"),
    };
    SHAPE_TEMPLATE
        .replace("{shape}", &shape)
        .replace("{n}", &target.to_string())
        .replace("{cap}", &cap.to_string())
        .replace("{nl}", if newline_ok { ", keeping its paragraph breaks" } else { ", with no line breaks" })
}

fn clause_task(op: &str, target: usize) -> String {
    match op {
        "paraphrase" => "Reword that part in different words, keeping its meaning.".into(),
        "polish" => "Polish that part: correct any grammar, spelling or punctuation and smooth awkward phrasing, keeping its meaning.".into(),
        "formalize" => "Rewrite that part in a more formal register, keeping its meaning.".into(),
        "casual" => "Rewrite that part in a relaxed, conversational way, keeping its meaning.".into(),
        "simplify" => "Rewrite that part in plainer, simpler words, keeping its meaning.".into(),
        "humanize" => "Reword that part so it reads naturally, the way a person would actually write it, keeping its meaning.".into(),
        "expand" => format!("Rewrite that part a little more fully, in about {target} words, elaborating only on what it already says."),
        _ => unreachable!("clause op {op}"),
    }
}

fn fit_text(start_rule: &str, end_rule: &str, clause: &str) -> String {
    let ws: Vec<&str> = clause.split(py_space).filter(|w| !w.is_empty()).collect();
    let (first, last) = (ws.first().copied().unwrap_or(""), ws.last().copied().unwrap_or(""));
    let start = match start_rule {
        "sentence_start" => format!("begin the sentence with a capital letter, as \"{first}\" does"),
        "class:lower" => format!("start with a lowercase letter, as \"{first}\" does"),
        "class:upper" => format!("start with a capital letter, as \"{first}\" does"),
        "class:digit" => format!("start with a digit, as \"{first}\" does"),
        _ => "read on from the words before it".to_owned(),
    };
    let end = match end_rule {
        r if r.starts_with("terminal:") => format!("end the sentence with its final punctuation mark ({})", &r[9..]),
        "terminal" => "end the sentence with its final punctuation mark".to_owned(),
        "no_punct" => "end without any punctuation mark".to_owned(),
        r if r.starts_with("same:") => {
            let c = &r[5..];
            let name = match c {
                "," => "a comma",
                ";" => "a semicolon",
                ":" => "a colon",
                _ => "a dash",
            };
            format!("end with {name} ({c}), as \"{last}\" does (the last character of your answer must be {name})")
        }
        _ => "end where the original part ends".to_owned(),
    };
    format!("{start}, and {end}")
}

fn char_class(c: char) -> &'static str {
    if c.is_uppercase() {
        "upper"
    } else if c.is_lowercase() {
        "lower"
    } else if c.is_ascii_digit() {
        "digit"
    } else {
        "other"
    }
}

// ---------------------------------------------------------------- plan

pub struct PlanInputs<'a> {
    pub train: &'a [Parent],
    pub dev: &'a [Parent],
    pub briefs: &'a [Brief],
    pub fix_slop: &'a str,
    pub train_target: usize,
    pub floor: usize,
    pub cap: usize,
    pub overprovision: f64,
    pub dev_variants: usize,
    pub tokenizer: &'a tokenizers::Tokenizer,
}

pub struct Plan {
    pub items: Vec<Item>,
    pub requests: Vec<Request>,
    pub failures: Vec<Value>,
    pub allocation: BTreeMap<String, Value>,
}

/// Square-root allocation with floor and cap; availability caps at
/// `max_per`. Water-filling on the scale, then largest remainder.
pub fn allocate(eligible: &BTreeMap<String, usize>, total: usize, floor: usize, cap: usize, max_per: usize) -> BTreeMap<String, usize> {
    let hi: BTreeMap<&String, f64> = eligible.iter().map(|(d, &e)| (d, (cap.min(max_per * e)) as f64)).collect();
    let lo: BTreeMap<&String, f64> = eligible.keys().map(|d| (d, (floor as f64).min(hi[d]))).collect();
    let sum_at = |lam: f64| -> f64 { eligible.iter().map(|(d, &e)| (lam * (e as f64).sqrt()).clamp(lo[d], hi[d])).sum() };
    let maxsum: f64 = hi.values().sum();
    let target = (total as f64).min(maxsum);
    let (mut a, mut b) = (0.0f64, 1.0f64);
    while sum_at(b) < target && b < 1e12 {
        b *= 2.0;
    }
    for _ in 0..200 {
        let m = 0.5 * (a + b);
        if sum_at(m) < target { a = m } else { b = m }
    }
    let raw: Vec<(&String, f64)> = eligible.iter().map(|(d, &e)| (d, (b * (e as f64).sqrt()).clamp(lo[d], hi[d]))).collect();
    let mut out: BTreeMap<String, usize> = raw.iter().map(|(d, x)| ((*d).clone(), x.floor() as usize)).collect();
    let mut left = (target.round() as usize).saturating_sub(out.values().sum());
    let mut rem: Vec<(f64, &String)> = raw.iter().map(|(d, x)| (x - x.floor(), *d)).collect();
    rem.sort_by(|x, y| y.0.total_cmp(&x.0).then(x.1.cmp(y.1)));
    for (_, d) in rem {
        if left == 0 {
            break;
        }
        if (out[d] as f64) < hi[d] {
            *out.get_mut(d).unwrap() += 1;
            left -= 1;
        }
    }
    out
}

struct Want {
    sort: String,
    list: (String, String, String, bool),
    item: usize,
    /// None: document-scope style shared by the item's style-capable edits
    edit: Option<usize>,
    field: &'static str,
}

fn tokens(tok: &tokenizers::Tokenizer, s: &str) -> usize {
    tok.encode(s, false).map(|e| e.get_ids().len()).unwrap_or(usize::MAX / 4)
}

fn genre_phrase(p: &Parent) -> String {
    let g = diverse::brief_genre(p.genre.as_deref().unwrap_or("personal_web"));
    format!("a passage of {}", crate::briefs::genre_desc(g).unwrap_or("everyday personal or web writing"))
}

pub fn build(inp: &PlanInputs) -> Result<Plan> {
    let cycle = policy_cycle();
    ensure!(cycle.len() == 50, "policy cycle");
    let mut items: Vec<Item> = Vec::new();
    let mut failures: Vec<Value> = Vec::new();
    let mut allocation: BTreeMap<String, Value> = BTreeMap::new();
    let parents: HashMap<&str, &Parent> = inp.train.iter().chain(inp.dev.iter()).map(|p| (p.id.as_str(), p)).collect();

    // ---- Train: domain allocation, parents by key, variants
    let mut by_dom: BTreeMap<String, Vec<&Parent>> = BTreeMap::new();
    for p in inp.train {
        by_dom.entry(p.domain.clone()).or_default().push(p);
    }
    let eligible: BTreeMap<String, usize> = by_dom.iter().map(|(d, v)| (d.clone(), v.len())).collect();
    let alloc = allocate(&eligible, inp.train_target, inp.floor, inp.cap, 2);
    // ranks for policy and generator within (split, domain)
    let mut slots: Vec<(f64, String, usize, Item)> = Vec::new();
    for (dom, ps) in &by_dom {
        let a = alloc[dom];
        let planned = ((a as f64 * inp.overprovision).round() as usize).min(2 * ps.len());
        let mut ps = ps.clone();
        ps.sort_by_key(|p| key("parent", &p.id));
        let (u, f) = if (planned as f64) <= 1.2 * ps.len() as f64 {
            (((planned as f64) / 1.2).ceil() as usize, 0.2)
        } else {
            (ps.len(), (planned as f64 / ps.len() as f64 - 1.0).min(1.0))
        };
        let mut seq: Vec<(&Parent, usize)> = Vec::new();
        for (j, p) in ps.iter().take(u).enumerate() {
            seq.push((p, 0));
            if ((j + 1) as f64 * f).floor() > (j as f64 * f).floor() {
                seq.push((p, 1));
            }
        }
        seq.truncate(planned);
        allocation.insert(dom.clone(), json!({"eligible_parents":ps.len(),"allocation_40k_rule":a,"planned_items":seq.len(),
            "parents_used":seq.iter().filter(|x| x.1 == 0).count(),"second_variants":seq.iter().filter(|x| x.1 == 1).count()}));
        let r_pol = rank_map(&ps, "policy");
        let r_gen = rank_map(&ps, "generator");
        let mut first_policy: HashMap<String, String> = HashMap::new();
        for (j, (p, v)) in seq.into_iter().enumerate() {
            let t = (j as f64 + 0.5) / a.max(1) as f64;
            let rp = r_pol[p.id.as_str()];
            let rg = r_gen[p.id.as_str()];
            let slot = if v == 0 { rp % 50 } else { (rp + 25) % 50 };
            let g = plan::TRAIN_INSTRUCT[if v == 0 { rg % 8 } else { (rg + 4) % 8 }];
            let g2 = plan::TRAIN_INSTRUCT[(if v == 0 { rg % 8 } else { (rg + 4) % 8 } + 1 + (ukey("second_gen", &format!("{}:{v}", p.id)) % 7) as usize) % 8];
            let avoid = if v == 1 { first_policy.get(&p.id).cloned() } else { None };
            match plan_item(p, TRAIN, v, &cycle, slot, g, g2, inp, avoid.as_deref()) {
                Ok(it) => {
                    if v == 0 {
                        first_policy.insert(p.id.clone(), it.policy.clone());
                    }
                    slots.push((t, dom.clone(), j, it))
                }
                Err(reason) => failures.push(json!({"schema":FAILURE_SCHEMA,"stage":"plan","population":TRAIN,"parent_id":p.id,"domain":p.domain,
                    "variant":v,"reason":reason})),
            }
        }
    }
    slots.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)).then(a.2.cmp(&b.2)));
    for (k, (t, _, _, mut it)) in slots.into_iter().enumerate() {
        it.order = k;
        it.order_t = t;
        items.push(it);
    }

    // ---- Dev-iter span-evaluation set: dev_variants per human
    let mut dslots: Vec<(f64, String, usize, Item)> = Vec::new();
    let mut dby: BTreeMap<String, Vec<&Parent>> = BTreeMap::new();
    for p in inp.dev {
        dby.entry(p.domain.clone()).or_default().push(p);
    }
    // halves: source groups ranked by key within domain, alternate
    let mut half: HashMap<String, &'static str> = HashMap::new();
    for ps in dby.values() {
        let mut groups: Vec<(String, &str)> = ps.iter().map(|p| (key("half", &p.source_group), p.source_group.as_str())).collect();
        groups.sort();
        groups.dedup();
        for (r, (_, g)) in groups.into_iter().enumerate() {
            half.insert(g.to_owned(), if r % 2 == 0 { "span_select" } else { "span_report" });
        }
    }
    for (dom, ps) in &dby {
        let mut ps = ps.clone();
        ps.sort_by_key(|p| key("parent", &p.id));
        let r_pol = rank_map(&ps, "dev_policy");
        let r_gen = rank_map(&ps, "dev_generator");
        let n_items = ps.len() * inp.dev_variants;
        let mut j = 0usize;
        for p in &ps {
            for v in 0..inp.dev_variants {
                let slot = (r_pol[p.id.as_str()] * inp.dev_variants + v) % 50;
                let gi = (r_gen[p.id.as_str()] * inp.dev_variants + v) % 7;
                let g = DEV_GENERATORS[gi];
                let g2 = DEV_GENERATORS[(gi + 1 + (ukey("second_gen", &format!("{}:{v}", p.id)) % 6) as usize) % 7];
                match plan_item(p, DEV_SET, v, &cycle, slot, g, g2, inp, None) {
                    Ok(mut it) => {
                        it.span_half = Some(half[&p.source_group].to_owned());
                        dslots.push(((j as f64 + 0.5) / n_items as f64, dom.clone(), j, it));
                    }
                    Err(reason) => failures.push(json!({"schema":FAILURE_SCHEMA,"stage":"plan","population":DEV_SET,"parent_id":p.id,
                        "domain":p.domain,"variant":v,"reason":reason})),
                }
                j += 1;
            }
        }
    }
    dslots.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)).then(a.2.cmp(&b.2)));
    for (k, (t, _, _, mut it)) in dslots.into_iter().enumerate() {
        it.order = k;
        it.order_t = t;
        items.push(it);
    }

    // ---- styles: per-document or per-edit, plan-v2 composition
    let lib = diverse::index_library(inp.briefs);
    let mut wants: Vec<Want> = Vec::new();
    let mut doc_style: HashMap<usize, Value> = HashMap::new();
    let mut by_pop_dom: BTreeMap<(String, String), Vec<usize>> = BTreeMap::new();
    for (i, it) in items.iter().enumerate() {
        by_pop_dom.entry((it.population.clone(), it.domain.clone())).or_default().push(i);
    }
    let mut doc_none_rank: HashMap<usize, usize> = HashMap::new();
    let mut doc_kind_rank: HashMap<usize, usize> = HashMap::new();
    for v in by_pop_dom.values() {
        for (tag, out) in [("style_none", &mut doc_none_rank), ("style_kind", &mut doc_kind_rank)] {
            let mut ks: Vec<(String, usize)> = v.iter().map(|&i| (key(tag, &items[i].item_id), i)).collect();
            ks.sort();
            for (r, (_, i)) in ks.into_iter().enumerate() {
                out.insert(i, r);
            }
        }
    }
    for (i, it) in items.iter_mut().enumerate() {
        let partition = if it.population == TRAIN { "train" } else { "dev_iter" };
        let p = parents[it.parent_id.as_str()];
        let bg = diverse::brief_genre(p.genre.as_deref().unwrap_or("personal_web")).to_owned();
        let sub = |owner: String| move |x: &str| -> usize { ukey(&format!("style:{x}"), &owner) as usize };
        let want_for = |kind: &str, owner: &str, edit: Option<usize>, wants: &mut Vec<Want>| {
            if matches!(kind, "voice" | "voice_plus" | "persona_combo") {
                wants.push(Want { sort: key("voice_order", owner), list: (partition.into(), "voice".into(), "any".into(), true), item: i, edit, field: "voice" });
            } else if kind == "brief" {
                wants.push(Want { sort: key("brief_order", owner), list: (partition.into(), "brief".into(), bg.clone(), true), item: i, edit, field: "brief" });
            }
        };
        let any_capable = it.edits.iter().any(|e| style_capable(&e.operation));
        if it.voice_scope == "document" && any_capable {
            let kind = diverse::style_kind(doc_none_rank[&i], doc_kind_rank[&i]);
            let mut st = diverse::style_fields(kind, &sub(it.item_id.clone()), false, &bg);
            st["style_scope"] = json!("document");
            want_for(kind, &it.item_id, None, &mut wants);
            doc_style.insert(i, st);
        }
        for e in it.edits.iter_mut() {
            if !style_capable(&e.operation) {
                e.style = json!({"style_scope":"none","style_kind":"operation_only"});
                continue;
            }
            let owner = format!("{}:{}", it.item_id, e.edit_index);
            let doc_kind = doc_style.get(&i).and_then(|s| s["style_kind"].as_str().map(String::from));
            match doc_kind {
                Some(k) if k != "none" => e.style = json!({"style_scope":"document"}),
                _ if it.voice_scope == "document" && e.operation != "voiced_paraphrase" => {
                    e.style = json!({"style_scope":"document","style_kind":"none"});
                }
                _ => {
                    let none = (ukey("edit_style_none", &owner) % 4) as usize;
                    let kr = (ukey("edit_style_kind", &owner) % 20) as usize;
                    let kind = if e.operation == "voiced_paraphrase" { diverse::style_kind(1, kr) } else { diverse::style_kind(none, kr) };
                    let mut st = diverse::style_fields(kind, &sub(owner.clone()), false, &bg);
                    st["style_scope"] = json!(if it.voice_scope == "document" { "edit_forced_voice" } else { "edit" });
                    want_for(kind, &owner, Some(e.edit_index), &mut wants);
                    e.style = st;
                }
            }
        }
    }
    wants.sort_by(|a, b| a.sort.cmp(&b.sort));
    let mut cursor: HashMap<(String, String, String, bool), usize> = HashMap::new();
    for w in &wants {
        let list = lib.get(&w.list).with_context(|| format!("no library items for {:?}", w.list))?;
        ensure!(!list.is_empty(), "empty library list {:?}", w.list);
        let c = cursor.entry(w.list.clone()).or_default();
        let fam = &items[w.item].source_group;
        let d = (0..list.len()).find(|d| list[(*c + d) % list.len()].source_family.as_deref() != Some(fam.as_str()))
            .context("every library item shares the parent's source family")?;
        let b = &list[(*c + d) % list.len()];
        *c += d + 1;
        let st = match w.edit {
            None => doc_style.get_mut(&w.item).unwrap(),
            Some(e) => &mut items[w.item].edits[e].style,
        };
        for (k, v) in [("id", json!(b.id)), ("text", json!(b.instruction)), ("inventor", json!(b.inventor)), ("axis", json!(b.axis)),
            ("partition", json!(b.partition)), ("imitated_author", json!(b.imitated_author)), ("cluster", json!(b.cluster)),
            ("origin", json!(b.origin)), ("source_family", json!(b.source_family))] {
            st[format!("{}_{k}", w.field)] = v;
        }
    }
    // resolve document styles into edits
    for (i, st) in &doc_style {
        for e in items[*i].edits.iter_mut() {
            if e.style["style_scope"] == "document" && e.style["style_kind"].is_null() {
                e.style = st.clone();
            }
        }
    }

    // ---- prompts and requests
    let mut requests = Vec::new();
    for it in items.iter_mut() {
        let p = parents[it.parent_id.as_str()];
        let doc = Doc::new(&p.text);
        let gd = genre_phrase(p);
        let item_id = it.item_id.clone();
        for e in it.edits.iter_mut() {
            let style = if style_capable(&e.operation) {
                let st = e.style.clone();
                let s = diverse::compose_style(&st, st["voice_text"].as_str(), st["brief_text"].as_str());
                if let Some(s) = &s {
                    e.style["style_text"] = json!(s);
                }
                s
            } else {
                None
            };
            ensure!(e.operation != "voiced_paraphrase" || style.is_some(), "voiced paraphrase without a voice: {}", it.item_id);
            let (s, t) = e.parent_span;
            let prompt = match e.kind.as_str() {
                "run" | "condense" => {
                    let span = &p.text[s..t];
                    let base_op = if e.operation == "voiced_paraphrase" { "paraphrase" } else { e.operation.as_str() };
                    let variant = (ukey("variant", &format!("{item_id}:{}", e.edit_index)) % 6) as usize;
                    let (pr, tw) = diverse::rewrite_prompt(base_op, span, "", &gd, style.as_deref(), variant, variant % 2 == 0, inp.fix_slop);
                    e.target_words = tw;
                    let (a, b) = e.sentence_range;
                    let shape = shape_sentence(base_op, b - a, tw, run_cap(base_op, e.source_words, tw), e.newline_ok, e.kind == "condense");
                    let context = e.context_included.then(|| ((a > 0).then(|| doc.sent(a - 1)), (b < doc.n()).then(|| doc.sent(b))));
                    compose_run(&pr, span, &shape, context)
                }
                "clause" => {
                    let i = e.sentence_range.0;
                    let sentence = doc.sent(i);
                    let (ctx_intro, block) = if e.context_included {
                        let before = if i > 0 { doc.sent(i - 1) } else { "(start of text)" };
                        let after = if i + 1 < doc.n() { doc.sent(i + 1) } else { "(end of text)" };
                        (", with the sentences around it for context", format!("BEFORE: {before}\nSENTENCE: {sentence}\nAFTER: {after}"))
                    } else {
                        ("", sentence.to_owned())
                    };
                    let task = clause_task(&e.operation, e.target_words);
                    CLAUSE_TEMPLATE
                        .replace("{ctx_intro}", ctx_intro)
                        .replace("{sentence_block}", &block)
                        .replace("{clause}", &p.text[s..t])
                        .replace("{task}", &task)
                        .replace("{style}", &style.as_ref().map(|x| format!(" {x}")).unwrap_or_default())
                        .replace("{n}", &e.target_words.to_string())
                        .replace("{fit}", &fit_text(&e.start_rule, &e.end_rule, &p.text[s..t]))
                }
                "insert" => {
                    let i = e.sentence_range.0;
                    let voice = match &style {
                        Some(sv) => format!("{sv} "),
                        None => "Match the voice and register of the text. ".to_owned(),
                    };
                    let n = e.target_words.to_string();
                    match e.operation.as_str() {
                        "insert_bridge" => BRIDGE_TEMPLATE
                            .replace("{genre}", &gd)
                            .replace("{first}", doc.sent(i))
                            .replace("{second}", doc.sent(i + 1))
                            .replace("{n}", &n)
                            .replace("{voice}", &voice),
                        "insert_elaborate" => {
                            let (intro, block) = if e.context_included && i > 0 {
                                (", with the sentence before it for context", format!("BEFORE: {}\nSENTENCE: {}", doc.sent(i - 1), doc.sent(i)))
                            } else {
                                ("", format!("SENTENCE: {}", doc.sent(i)))
                            };
                            ELABORATE_TEMPLATE
                                .replace("{genre}", &gd)
                                .replace("{ctx_intro}", intro)
                                .replace("{sentence_block}", &block)
                                .replace("{n}", &n)
                                .replace("{voice}", &voice)
                        }
                        _ => CONTINUE_TEMPLATE.replace("{genre}", &gd).replace("{text}", p.text.trim_end()).replace("{n}", &n).replace("{voice}", &voice),
                    }
                }
                k => bail!("edit kind {k}"),
            };
            let max_tokens = (3 * e.target_words + 64).clamp(128, 6000) as u32;
            let mut d: Decoding = diverse::sample_decoding(&format!("{item_id}:{}", e.generator), &e.generator, false);
            d.profile = "spans_v1_sampled".into();
            d.max_tokens = max_tokens;
            let rs = plan::request_sha(&e.generator, "chat", &e.template_id, &prompt, &d);
            let rid = format!("sreq-{}", &text::sha_hex(format!("{item_id}:{}", e.edit_index).as_bytes())[..24]);
            e.request_id = rid.clone();
            requests.push(Request {
                request_id: rid,
                item_id: item_id.clone(),
                parent_id: it.parent_id.clone(),
                split: it.split.clone(),
                generator: e.generator.clone(),
                endpoint: "chat".into(),
                template_id: e.template_id.clone(),
                prompt,
                span_index: e.edit_index,
                source_span: Some(e.parent_span),
                target_words: e.target_words,
                cut_budget: None,
                decoding: d,
                seed: plan::seed_of(&rs),
                request_sha256: rs,
                op: Some(e.operation.clone()),
                cohort: Some(format!("{COHORT_ID}:{}", it.population)),
                attempt_limit: None,
            });
        }
    }
    Ok(Plan { items, requests, failures, allocation })
}

fn rank_map<'a>(ps: &[&'a Parent], tag: &str) -> HashMap<&'a str, usize> {
    let mut v: Vec<(String, &str)> = ps.iter().map(|p| (key(tag, &p.id), p.id.as_str())).collect();
    v.sort();
    v.into_iter().enumerate().map(|(r, (_, id))| (id, r)).collect()
}

/// One item: policy (keyed slot; plan-time feasibility fallback along the
/// cycle, never after any generation), units, operations and generators.
#[allow(clippy::too_many_arguments)]
fn plan_item(p: &Parent, population: &str, v: usize, cycle: &[&'static str], slot: usize, g: &str, g2: &str, inp: &PlanInputs, avoid: Option<&str>) -> Result<Item, String> {
    let split = if population == TRAIN { "train" } else { "dev_iter" };
    let item_id = format!("splice-{}", &text::sha_hex(format!("{KEY_PREFIX}item:{population}:{}:{v}", p.id).as_bytes())[..24]);
    let doc = Doc::new(&p.text);
    let requested = cycle[slot];
    // the second Train variant avoids the first variant's final policy
    let mut chosen = None;
    for step in 0..50 {
        let pol = cycle[(slot + step) % 50];
        if avoid == Some(pol) {
            continue;
        }
        if let Some(units) = select_policy(pol, &doc, &item_id) {
            chosen = Some((pol, step, units));
            break;
        }
    }
    let Some((policy, steps, units)) = chosen else { return Err("no_feasible_policy".into()) };
    let two = units.len() >= 2 && ukey("two_generators", &item_id) % 10 == 0;
    let voice_scope = if ukey("voice_scope", &item_id) % 10 < 7 { "document" } else { "edit" };
    let parent_tokens = p.a_tokens;
    let mut edits = Vec::new();
    for (k, (component, unit)) in units.into_iter().enumerate() {
        let owner = format!("{item_id}:{k}");
        let generator = if two && k % 2 == 1 { g2 } else { g }.to_owned();
        let context_included = ukey("context", &owner) % 2 == 0;
        let mut e = Edit {
            edit_index: k,
            component: component.clone(),
            kind: String::new(),
            operation: String::new(),
            family: String::new(),
            operation_requested: String::new(),
            generator,
            parent_span: (0, 0),
            insert_sep: None,
            sentence_range: (0, 0),
            context_included,
            context_spans: vec![],
            style: Value::Null,
            template_id: String::new(),
            source_words: 0,
            target_words: 0,
            len_lo: 0.0,
            len_hi: 0.0,
            min_change: None,
            start_rule: "any".into(),
            end_rule: "any".into(),
            newline_ok: false,
            request_id: String::new(),
        };
        match unit {
            Unit::Run { a, b } | Unit::Condense { a, b } => {
                let condense_policy = matches!(unit, Unit::Condense { .. });
                let (s, t) = doc.span(a, b);
                let span = &p.text[s..t];
                let w = py_words(span);
                let req = if condense_policy { "condense" } else { slot_pick(&RUN_SLOTS, ukey("op", &owner)) };
                let mut op = req;
                if op == "fix_slop" && w > 150 {
                    op = "polish";
                }
                if op == "expand" && parent_tokens + (13 * tokens(inp.tokenizer, span)) / 10 > 2000 {
                    op = "paraphrase";
                }
                if op == "condense" && w < 8 {
                    op = if condense_policy { return Err("condense_run_too_short".into()) } else { "paraphrase" };
                }
                e.kind = if condense_policy { "condense" } else { "run" }.into();
                e.operation_requested = req.into();
                e.operation = op.into();
                e.parent_span = (s, t);
                e.sentence_range = (a, b);
                if e.context_included {
                    if a > 0 {
                        e.context_spans.push((doc.sents[a - 1].start, doc.sents[a - 1].end));
                    }
                    if b < doc.n() {
                        e.context_spans.push((doc.sents[b].start, doc.sents[b].end));
                    }
                }
                e.template_id = if e.context_included { T_RUN_CTX } else { T_RUN }.into();
                e.source_words = w;
                let (lo, hi) = match op {
                    "expand" => (1.2, 2.2),
                    "condense" => (0.3, 0.8),
                    _ => (0.5, 1.5),
                };
                e.len_lo = (lo * w as f64).max(1.0);
                e.len_hi = hi * w as f64;
                e.min_change = Some(min_change(op));
                e.start_rule = "sentence_start".into();
                e.end_rule = if doc.sents[b - 1].punctuated { "terminal" } else { "any" }.into();
                e.newline_ok = span.contains('\n');
            }
            Unit::Clause { sent, s, e: t } => {
                let span = &p.text[s..t];
                let w = py_words(span);
                let req = slot_pick(&CLAUSE_SLOTS, ukey("op", &owner));
                let mut op = req;
                let mut target = w;
                if op == "expand" {
                    target = ((w as f64) * 1.5).round() as usize;
                    if target > 36 {
                        op = "paraphrase";
                        target = w;
                    }
                }
                e.kind = "clause".into();
                e.operation_requested = req.into();
                e.operation = op.into();
                e.parent_span = (s, t);
                e.sentence_range = (sent, sent + 1);
                e.context_spans.push((doc.sents[sent].start, doc.sents[sent].end));
                if e.context_included {
                    if sent > 0 {
                        e.context_spans.push((doc.sents[sent - 1].start, doc.sents[sent - 1].end));
                    }
                    if sent + 1 < doc.n() {
                        e.context_spans.push((doc.sents[sent + 1].start, doc.sents[sent + 1].end));
                    }
                }
                e.template_id = T_CLAUSE.into();
                e.source_words = w;
                e.target_words = target;
                e.len_lo = 2.0;
                e.len_hi = 40.0;
                e.min_change = Some(min_change(op));
                let first = span.chars().next().unwrap();
                e.start_rule = if s == doc.sents[sent].start { "sentence_start".into() } else { format!("class:{}", char_class(first)) };
                let last = span.chars().last().unwrap();
                e.end_rule = if is_clause_punct(last) {
                    format!("same:{last}")
                } else if t == doc.sents[sent].end && doc.sents[sent].punctuated {
                    let mark = span.trim_end_matches(is_closer).chars().last().filter(|c| is_terminal(*c)).unwrap_or('.');
                    format!("terminal:{mark}")
                } else {
                    "no_punct".into()
                };
                e.newline_ok = false;
            }
            Unit::Insert { after, at_end } => {
                let pos = doc.sents[after].end;
                let op = if at_end {
                    "continue"
                } else if ukey("insert_op", &owner) % 2 == 0 {
                    "insert_bridge"
                } else {
                    "insert_elaborate"
                };
                let mean = (p.words as f64 / doc.n() as f64).round() as usize;
                e.kind = "insert".into();
                e.operation_requested = op.into();
                e.operation = op.into();
                e.parent_span = (pos, pos);
                e.insert_sep = Some(if at_end && p.text.contains("\n\n") { "\n\n" } else { " " }.into());
                e.sentence_range = (after, after + 1);
                e.context_included = true;
                e.context_spans = match op {
                    "insert_bridge" => vec![(doc.sents[after].start, doc.sents[after].end), (doc.sents[after + 1].start, doc.sents[after + 1].end)],
                    "insert_elaborate" => {
                        let mut v = vec![(doc.sents[after].start, doc.sents[after].end)];
                        if ukey("context", &owner) % 2 == 0 && after > 0 {
                            v.push((doc.sents[after - 1].start, doc.sents[after - 1].end));
                        }
                        v
                    }
                    _ => vec![(0, p.text.len())],
                };
                e.template_id = match op {
                    "insert_bridge" => T_BRIDGE,
                    "insert_elaborate" => T_ELABORATE,
                    _ => T_CONTINUE,
                }
                .into();
                e.source_words = 0;
                e.target_words = mean.clamp(8, 35);
                e.len_lo = 6.0;
                e.len_hi = 60.0;
                e.min_change = None;
                let neighbour = if at_end { doc.sent(after) } else { doc.sent(after + 1) };
                e.start_rule = if neighbour.chars().next().is_some_and(|c| c.is_uppercase() || c.is_ascii_digit() || is_opener(c)) {
                    "sentence_start_required".into()
                } else {
                    "any".into()
                };
                e.end_rule = "terminal".into();
                e.newline_ok = false;
            }
        }
        // insert_elaborate keeps its own context flag (prompt shows the previous sentence when set)
        if e.kind == "insert" && e.operation == "insert_elaborate" {
            e.context_included = e.context_spans.len() > 1;
        } else if e.kind == "insert" {
            e.context_included = true;
        }
        e.family = family_of(&e.operation).into();
        edits.push(e);
    }
    let mut gens: Vec<String> = edits.iter().map(|e| e.generator.clone()).collect();
    gens.sort();
    gens.dedup();
    Ok(Item {
        item_id,
        population: population.into(),
        split: split.into(),
        domain: p.domain.clone(),
        parent_id: p.id.clone(),
        parent_text_sha256: p.text_sha256.clone(),
        source_group: p.source_group.clone(),
        writer_key: p.writer_key.clone(),
        variant: v,
        order: 0,
        order_t: 0.0,
        policy: policy.into(),
        policy_requested: requested.into(),
        policy_fallback_steps: steps,
        two_generators: gens.len() > 1,
        generators: gens,
        voice_scope: voice_scope.into(),
        span_half: None,
        edits,
    })
}

// ---------------------------------------------------------------- verdict

pub const LEADING_META: [&str; 14] = [
    "sure", "certainly", "here is", "here's", "below is", "i can't", "i cannot", "i'm sorry", "as an ai", "rewritten", "revised", "paraphrased", "output:", "note:",
];
pub const ANYWHERE_META: [&str; 7] = ["rewritten sentence", "revised version", "the passage", "this passage", "the original", "changes made", "(note"];
/// Prompt labels leaking into an output (case-sensitive; exempt when the parent contains them). Pilot amendment D1.
pub const PROMPT_LABELS: [&str; 11] =
    ["BEFORE:", "AFTER:", "SENTENCE:", "FIRST:", "SECOND:", "PASSAGE", "BEGINNING:", "(start of text)", "(end of text)", "⟦", "⟧"];
/// Assistant phrasing (case-insensitive; exempt when the parent contains them). Pilot amendment D1.
pub const EXTRA_META: [&str; 22] = [
    "here's the", "here is the", "here's a ", "here is a ", "here's my", "here is my", "let me know", "i hope this", "as requested", "the context",
    "provided text", "given text", "original text", "the text provided", "the sentence provided", "given sentence", "word count", "rewritten version",
    "rewritten text", "revised text", "this part of the sentence", "the marked part",
];

fn norm_quotes(s: &str) -> String {
    s.replace(['’', '‘'], "'").replace(['“', '”'], "\"").to_lowercase()
}

fn markdown_marks(s: &str) -> Vec<&'static str> {
    let mut v = Vec::new();
    for line in s.lines() {
        let l = line.trim_start();
        if l.starts_with('#') {
            v.push("heading");
        }
        if l.starts_with("- ") || l.starts_with("* ") || l.starts_with("• ") {
            v.push("bullet");
        }
        let digits = l.chars().take_while(|c| c.is_ascii_digit()).count();
        if digits > 0 && l[digits..].starts_with(". ") {
            v.push("numbered_list");
        }
    }
    if s.contains("**") || s.contains("__") {
        v.push("bold");
    }
    if s.contains("```") {
        v.push("code_fence");
    }
    v.sort();
    v.dedup();
    v
}

fn ends_terminal(s: &str) -> bool {
    let t = s.trim_end_matches(is_closer);
    t.chars().last().is_some_and(is_terminal)
}

/// Acceptance checks for one spliced edit (protocol Section 3.6).
pub fn verdict(edit: &Edit, parent: &str, overlap: &Overlap, status: u16, parsed: &Value) -> Value {
    let mut reasons: Vec<String> = Vec::new();
    let mut diag = json!({});
    if status != 200 {
        reasons.push(format!("http_status_{status}"));
    }
    let finish = parsed["choices"][0]["finish_reason"].as_str().map(str::to_owned);
    diag["finish_reason"] = json!(finish);
    if finish.as_deref() == Some("length") {
        reasons.push("truncated_finish_length".into());
    }
    let mut out = json!({"accepted":false});
    let clean = match parsed["choices"][0]["message"]["content"].as_str() {
        None => {
            reasons.push("no_content".into());
            None
        }
        Some(c) => match text::strip_empty_think(c) {
            Err(e) => {
                reasons.push(e.into());
                None
            }
            Ok(t) => Some(t.trim().to_owned()),
        },
    };
    if let Some(clean) = clean {
        let (s, e) = edit.parent_span;
        let source = &parent[s..e];
        let mut exempt = source.to_owned();
        for &(a, b) in &edit.context_spans {
            exempt.push('\n');
            exempt.push_str(&parent[a..b]);
        }
        let ex = norm_quotes(&exempt);
        let low = norm_quotes(&clean);
        if clean.is_empty() {
            reasons.push("empty_content".into());
        }
        let sp = text::special_tokens(&clean);
        if !sp.is_empty() {
            reasons.push("chat_template_special_tokens".into());
        }
        if clean.chars().any(|c| (c < ' ' && c != '\n' && c != '\t' && !(c == '\r' && source.contains('\r'))) || c == '\u{7f}' || ('\u{1c}'..='\u{1f}').contains(&c)) {
            reasons.push("control_characters".into());
        }
        for m in LEADING_META {
            if low.starts_with(m) && (m.ends_with(':') || !low[m.len()..].starts_with(|c: char| c.is_alphanumeric())) && !ex.trim_start().starts_with(m) {
                reasons.push(format!("meta_leading:{m}"));
            }
        }
        for m in ANYWHERE_META {
            if low.contains(m) && !ex.contains(m) {
                reasons.push(format!("meta_anywhere:{m}"));
            }
        }
        let parent_low = norm_quotes(parent);
        for m in PROMPT_LABELS {
            if clean.contains(m) && !parent.contains(m) {
                reasons.push(format!("prompt_label:{m}"));
            }
        }
        for m in EXTRA_META {
            if low.contains(m) && !parent_low.contains(m) {
                reasons.push(format!("meta_phrase:{}", m.trim()));
            }
        }
        // spans-v1.1 meta screen v2 (labels, prefaces, prompt/skill echoes, persona labels, word counts, scripts)
        reasons.extend(crate::splice_amend::meta_screen(&clean, parent));
        let md: Vec<&str> = markdown_marks(&clean).into_iter().filter(|m| !markdown_marks(&exempt).contains(m)).collect();
        if !md.is_empty() {
            reasons.push(format!("markdown:{}", md.join("+")));
        }
        let trimmed_end = clean.trim_end_matches(|c: char| matches!(c, '.' | ',' | ';' | ':' | '!' | '?'));
        let opens = clean.chars().next().is_some_and(|c| matches!(c, '"' | '“' | '\'' | '‘' | '«'));
        let closes = trimmed_end.chars().last().is_some_and(|c| matches!(c, '"' | '”' | '\'' | '’' | '»'));
        if opens && closes && !source.trim_start().starts_with(|c: char| matches!(c, '"' | '“' | '\'' | '‘' | '«')) && clean.chars().count() > 2 {
            reasons.push("enclosing_quotes_added".into());
        }
        let w = py_words(&clean);
        diag["words"] = json!(w);
        diag["source_words"] = json!(edit.source_words);
        if (w as f64) < edit.len_lo || (w as f64) > edit.len_hi {
            reasons.push(format!("length_outside_{}", edit.kind));
        }
        match edit.end_rule.as_str() {
            r if r.starts_with("terminal") && !ends_terminal(&clean) => reasons.push("missing_terminal_punctuation".into()),
            "no_punct" if clean.chars().last().is_some_and(|c| is_terminal(c) || is_clause_punct(c)) => reasons.push("clause_end_punctuation_added".into()),
            r if r.starts_with("same:") && !clean.ends_with(&r[5..]) => reasons.push("clause_end_punctuation_changed".into()),
            _ => {}
        }
        let first = clean.chars().next();
        match edit.start_rule.as_str() {
            "sentence_start" => {
                let src_ok = source.chars().next().is_some_and(|c| c.is_uppercase() || c.is_ascii_digit() || is_opener(c));
                if src_ok && !first.is_some_and(|c| c.is_uppercase() || c.is_ascii_digit() || is_opener(c)) {
                    reasons.push("sentence_start_case".into());
                }
            }
            "sentence_start_required" => {
                if !first.is_some_and(|c| c.is_uppercase() || c.is_ascii_digit() || is_opener(c)) {
                    reasons.push("sentence_start_case".into());
                }
            }
            r if r.starts_with("class:") => {
                let want = &r[6..];
                if want != "other" && first.map(char_class) != Some(want) {
                    reasons.push("clause_start_class_changed".into());
                }
            }
            _ => {}
        }
        if clean.contains(['\n', '\r']) && !edit.newline_ok {
            reasons.push("newline_not_in_replaced_span".into());
        }
        if first.is_some_and(|c| matches!(c, ',' | ';' | ':' | '.' | '!' | '?' | ')')) && !source.starts_with(first.unwrap()) {
            reasons.push("leading_punctuation".into());
        }
        let tail: Vec<char> = clean.chars().rev().take(2).collect();
        if tail.len() == 2 {
            let (last, prev) = (tail[0], tail[1]);
            let pc = |c: char| matches!(c, ',' | ';' | ':');
            let tc = |c: char| matches!(c, '.' | '!' | '?');
            if (pc(last) && pc(prev)) || (pc(last) && tc(prev)) || (tc(last) && pc(prev)) {
                reasons.push("doubled_punctuation".into());
            }
        }
        if edit.kind != "insert" {
            if clean == source {
                reasons.push("byte_identical_to_replaced_span".into());
            }
            let cf = diverse::change_fraction(source, &clean);
            diag["change_fraction"] = json!(cf);
            if let Some(m) = edit.min_change {
                if cf < m {
                    reasons.push("change_fraction_below_min".into());
                }
            }
            // repeats of read-only context that the source span does not contain
            if edit.kind == "clause" {
                let (ss, se) = edit.context_spans[0];
                let rest = format!("{} {}", &parent[ss..s], &parent[e..se]);
                let srcw: HashSet<u64> = text::folded_windows(source, 4).into_iter().collect();
                if text::folded_windows(&clean, 4).into_iter().any(|x| !srcw.contains(&x) && text::folded_windows(&rest, 4).contains(&x)) {
                    reasons.push("repeats_rest_of_sentence".into());
                }
            } else {
                let srcw: HashSet<u64> = text::folded_windows(source, 8).into_iter().collect();
                for &(a, b) in &edit.context_spans {
                    let cw: HashSet<u64> = text::folded_windows(&parent[a..b], 8).into_iter().collect();
                    if text::folded_windows(&clean, 8).into_iter().any(|x| !srcw.contains(&x) && cw.contains(&x)) {
                        reasons.push("repeats_context".into());
                        break;
                    }
                }
            }
        } else {
            if crate::overlap::shares_window(&clean, parent, 12) {
                reasons.push("copies_parent_12_word_window".into());
            }
            for &(a, b) in edit.context_spans.iter().filter(|r| r.1 - r.0 < parent.len()) {
                if crate::overlap::shares_window(&clean, &parent[a..b], 8) {
                    reasons.push("repeats_neighbour_8_word_window".into());
                    break;
                }
            }
        }
        let conflicts = overlap.conflicts(&clean, Some(parent));
        if !conflicts.is_empty() {
            reasons.push("duplicate_or_prior_overlap".into());
            diag["overlap"] = json!(conflicts);
        }
        reasons.sort();
        reasons.dedup();
        out["text"] = json!(clean);
    }
    out["accepted"] = json!(reasons.is_empty());
    out["reasons"] = json!(reasons);
    out["diag"] = diag;
    out
}

/// Verdict function for `generation::run_with`.
pub fn verdict_fn(edits: HashMap<String, Edit>) -> std::sync::Arc<crate::generation::VerdictFn> {
    std::sync::Arc::new(move |ctx: &Ctx, r: &Request, status: u16, parsed: &Value| -> Value {
        match (edits.get(&r.request_id), ctx.parents.get(&r.parent_id)) {
            (Some(e), Some(p)) => verdict(e, p, &ctx.overlap, status, parsed),
            _ => json!({"accepted":false,"reasons":["edit_or_parent_missing"],"diag":{}}),
        }
    })
}

// ---------------------------------------------------------------- journals

/// Journal states without raw bodies or prompts (memory-bounded).
pub fn load_states_light(dirs: &[PathBuf]) -> Result<HashMap<String, ReqState>> {
    let mut m: HashMap<String, ReqState> = HashMap::new();
    for d in dirs {
        for p in crate::generation::journal_files(d)? {
            let f = std::io::BufReader::new(std::fs::File::open(&p)?);
            for line in f.lines() {
                let line = line?;
                if line.trim().is_empty() {
                    continue;
                }
                let Ok(mut v) = serde_json::from_str::<Value>(&line) else {
                    eprintln!("torn journal line in {}", p.display());
                    continue;
                };
                let id = v["request_id"].as_str().context("request_id")?.to_owned();
                let a = v["attempt"].as_u64().context("attempt")? as u32;
                let st = m.entry(id).or_default();
                match v["kind"].as_str() {
                    Some("intent") => st.intents.push(a),
                    Some("response") => {
                        if let Some(o) = v.as_object_mut() {
                            o.remove("raw_body");
                        }
                        st.responses.insert(a, v);
                    }
                    _ => bail!("journal kind"),
                }
            }
        }
    }
    Ok(m)
}

pub fn run_dirs(root: &Path) -> Result<Vec<PathBuf>> {
    let mut v = Vec::new();
    if root.exists() {
        for e in std::fs::read_dir(root)? {
            let p = e?.path();
            if p.is_dir() {
                v.push(p);
            }
        }
    }
    v.sort();
    Ok(v)
}

// ---------------------------------------------------------------- assembly

#[derive(Debug, Clone, PartialEq)]
pub struct Assembled {
    pub text: String,
    pub ai_byte_spans: Vec<(usize, usize)>,
    /// final byte span per edit, in edit order
    pub edit_spans: Vec<(usize, usize)>,
    pub ai_words: usize,
    pub total_words: usize,
}

pub fn char_index(text: &str, byte: usize) -> usize {
    text[..byte].chars().count()
}

/// Splice accepted replacements into the parent. Edits must be disjoint;
/// insertions add their separator before the new text. Verifies the human
/// characters, whitespace boundaries and word accounting mechanically.
pub fn assemble(parent: &str, edits: &[&Edit], reps: &[&str]) -> Result<Assembled> {
    let parts: Vec<Part> = edits.iter().map(|e| Part { parent_span: e.parent_span, insert_sep: e.insert_sep.as_deref() }).collect();
    assemble_parts(parent, &parts, reps)
}

/// `assemble` for laid-out profile v2 replacements (`assemble_laid_parts`).
pub fn assemble_laid(parent: &str, edits: &[&Edit], reps: &[&str]) -> Result<Assembled> {
    let parts: Vec<Part> = edits.iter().map(|e| Part { parent_span: e.parent_span, insert_sep: e.insert_sep.as_deref() }).collect();
    assemble_laid_parts(parent, &parts, reps)
}

/// The two fields of an edit that assembly reads (from a plan edit or a record's edit history).
#[derive(Debug, Clone, Copy)]
pub struct Part<'a> {
    pub parent_span: (usize, usize),
    pub insert_sep: Option<&'a str>,
}

/// The byte length of a replacement's leading whitespace when it is of the
/// one form assembly accepts: none, or one line break followed by a lead
/// with no other line break (a profile v2 edit whose wrapped first line has
/// no room on the line it shares with an earlier edit; protocol A2
/// amendment 29 item 29.1, `grammar_a_form::profile::replacement_lead`).
/// The line break and lead belong to no edit's final span.
pub fn replacement_lead(r: &str) -> Option<usize> {
    let lead = r.len() - r.trim_start_matches(py_space).len();
    (lead == 0 || (r.starts_with('\n') && !r[1..lead].contains('\n'))).then_some(lead)
}

/// The trailing whitespace a laid-out profile v2 replacement may hold
/// (protocol A2 amendment 42 item 42.2; `grammar_a_form::profile::
/// replacement_tail`, pinned by `grammar-cc-human`'s parity test): none, or,
/// after a boundary break, the parent's whitespace run after the edit
/// (non-empty, no line break), one line break and a lead with no line break.
/// Returns (the run's, the whole trailing whitespace's) byte lengths, (0, 0)
/// for none, or None for trailing whitespace of any other form.
pub fn replacement_tail(r: &str) -> Option<(usize, usize)> {
    let t = &r[r.trim_end_matches(py_space).len()..];
    if t.is_empty() {
        return Some((0, 0));
    }
    let k = t.find('\n')?;
    (k > 0 && !t[k + 1..].contains('\n')).then_some((k, t.len()))
}

/// Amendment 42 item 42.2's check inside an edit's extended range: the run
/// a replacement keeps before its boundary break is the parent's bytes at
/// `e` exactly and the parent's whole run of non-line-break whitespace there
/// (each character Unicode whitespace), followed on the line by a parent
/// byte that is not whitespace, before the next edit's parent start `next`.
/// Returns the run's byte length (0 without trailing whitespace).
pub(crate) fn extended_run(parent: &str, e: usize, r: &str, next: usize) -> Result<usize> {
    let (run, len) = replacement_tail(r).context("replacement layout")?;
    if len == 0 {
        return Ok(0);
    }
    let kept = &r[r.len() - len..r.len() - len + run];
    let after = &parent[e..];
    let rest = after.trim_start_matches(|c: char| py_space(c) && c != '\n');
    let whole = after.len() - rest.len() == run && kept.chars().all(char::is_whitespace) && rest.chars().next().is_some_and(|c| !py_space(c));
    ensure!(after.starts_with(kept) && whole && e + run < next, "human characters changed");
    Ok(run)
}

/// `assemble` over parent spans and insertion separators, for raw responses
/// (the raw twin and the screen-fail stratum) and any text that is not a
/// laid-out profile v2 child. A replacement is non-empty and has no trailing
/// whitespace; its leading whitespace is `replacement_lead`'s, outside its
/// final span. Unchanged by amendment 42 (item 42.2: the raw twin's assembly
/// reads raw responses).
pub fn assemble_parts(parent: &str, edits: &[Part], reps: &[&str]) -> Result<Assembled> {
    assemble_parts_as(parent, edits, reps, false)
}

/// `assemble_parts` for laid-out profile v2 replacements (`grammar_a_form::
/// profile::assemble_item`'s rules, pinned by `grammar-cc-human`'s parity
/// test): a replacement may also end with a boundary break
/// (`replacement_tail`; protocol A2 amendment 42 item 42.2). Its edit's range
/// is then read as ending after the parent's whitespace run it keeps (its
/// extended range): the human characters are the parent's bytes outside the
/// extended ranges, unchanged and in order, and inside one the bytes after
/// the edit's laid-out text are exactly that run, one line break and a lead
/// with no line break. The run, the line break and the lead lie outside the
/// edit's final span, which ends at its last word; no final span starts or
/// ends in whitespace. Any other trailing whitespace is refused as
/// `assemble_parts` refuses it.
pub fn assemble_laid_parts(parent: &str, edits: &[Part], reps: &[&str]) -> Result<Assembled> {
    assemble_parts_as(parent, edits, reps, true)
}

fn assemble_parts_as(parent: &str, edits: &[Part], reps: &[&str], laid: bool) -> Result<Assembled> {
    ensure!(edits.len() == reps.len() && !edits.is_empty(), "edit/replacement count");
    let mut order: Vec<usize> = (0..edits.len()).collect();
    order.sort_by_key(|&i| (edits[i].parent_span.0, edits[i].parent_span.1));
    let mut out = String::new();
    let mut human = String::new();
    let mut cur = 0usize;
    let mut spans = vec![(0usize, 0usize); edits.len()];
    // each edit's extended parent range end (amendment 42 item 42.2)
    let mut ends = vec![0usize; edits.len()];
    for (k, &i) in order.iter().enumerate() {
        let (s, e) = edits[i].parent_span;
        let r = reps[i];
        ensure!(s >= cur && s <= e && e <= parent.len(), "edit order or overlap");
        ensure!(parent.is_char_boundary(s) && parent.is_char_boundary(e), "char boundary");
        let lead = replacement_lead(r).filter(|_| !r.trim_matches(py_space).is_empty());
        ensure!(lead.is_some(), "replacement layout");
        let tail = if laid { replacement_tail(r).context("replacement layout")?.1 } else { 0 };
        ensure!(laid || r.trim_end_matches(py_space) == r, "replacement layout");
        let seg = &parent[cur..s];
        out.push_str(seg);
        human.push_str(seg);
        if s == e {
            out.push_str(edits[i].insert_sep.context("insert separator")?);
        } else {
            ensure!(s == 0 || parent[..s].ends_with(py_space), "left boundary");
            ensure!(e == parent.len() || parent[e..].starts_with(py_space), "right boundary");
        }
        let next = order.get(k + 1).map_or(usize::MAX, |&j| edits[j].parent_span.0);
        let run = if laid { extended_run(parent, e, r, next)? } else { 0 };
        let fs = out.len() + lead.unwrap_or(0);
        out.push_str(r);
        spans[i] = (fs, out.len() - tail);
        cur = e + run;
        ends[i] = cur;
    }
    out.push_str(&parent[cur..]);
    human.push_str(&parent[cur..]);
    // human characters outside the (extended) spans equal the parent minus
    // the replaced (extended) spans
    let mut expect = String::new();
    let mut c = 0;
    for &i in &order {
        expect.push_str(&parent[c..edits[i].parent_span.0]);
        c = ends[i];
    }
    expect.push_str(&parent[c..]);
    ensure!(human == expect, "human characters changed");
    let mut sorted = spans.clone();
    sorted.sort();
    for w in sorted.windows(2) {
        ensure!(w[0].1 < w[1].0, "final spans touch or overlap");
    }
    for &(a, b) in &sorted {
        ensure!(a == 0 || out[..a].ends_with(py_space), "final left boundary");
        ensure!(b == out.len() || out[b..].starts_with(py_space), "final right boundary");
        ensure!(!out[a..b].starts_with(py_space) && !out[a..b].ends_with(py_space), "span edge whitespace");
    }
    let words = py_word_ranges(&out);
    let mut inside = 0usize;
    for &(a, b) in &words {
        let k = sorted.iter().filter(|&&(s, e)| a >= s && b <= e).count();
        let straddle = sorted.iter().any(|&(s, e)| (a < s && b > s) || (a < e && b > e));
        ensure!(!straddle, "word straddles a span edge");
        inside += k;
    }
    let ai_words: usize = reps.iter().map(|r| py_words(r)).sum();
    ensure!(inside == ai_words, "word accounting");
    ensure!(!words.is_empty() && inside < words.len(), "document has no human word left");
    Ok(Assembled { text: out, ai_byte_spans: sorted, edit_spans: spans, ai_words, total_words: words.len() })
}

pub fn share_bin(s: f64) -> &'static str {
    crate::finalize::share_bin(s)
}

pub fn cf_band(cf: f64) -> &'static str {
    if cf < 0.1 {
        "<0.1"
    } else if cf < 0.3 {
        "0.1-0.3"
    } else {
        ">=0.3"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn edit(kind: &str, span: (usize, usize), sep: Option<&str>) -> Edit {
        Edit {
            edit_index: 0,
            component: "x".into(),
            kind: kind.into(),
            operation: "paraphrase".into(),
            family: "paraphrase".into(),
            operation_requested: "paraphrase".into(),
            generator: "g".into(),
            parent_span: span,
            insert_sep: sep.map(String::from),
            sentence_range: (0, 1),
            context_included: false,
            context_spans: vec![],
            style: Value::Null,
            template_id: T_RUN.into(),
            source_words: 3,
            target_words: 3,
            len_lo: 1.0,
            len_hi: 10.0,
            min_change: Some(0.1),
            start_rule: "sentence_start".into(),
            end_rule: "terminal".into(),
            newline_ok: false,
            request_id: "r".into(),
        }
    }

    #[test]
    fn python_whitespace_rule() {
        assert!(py_space('\u{1c}') && py_space('\u{a0}') && py_space('\n') && !py_space('x'));
        assert_eq!(py_words("a\u{1f}b  c\u{a0}d"), 4);
        assert_eq!(py_word_ranges(" ab c"), vec![(1, 3), (4, 5)]);
    }

    #[test]
    fn assemble_replace_insert_and_exact_accounting() {
        let p = "One two three. Four five six. Seven eight nine.";
        let d = Doc::new(p);
        let e1 = edit("run", d.span(1, 2), None);
        let e2 = edit("insert", (d.sents[2].end, d.sents[2].end), Some(" "));
        let a = assemble(p, &[&e1, &e2], &["Quatre cinq.", "Ten eleven."]).unwrap();
        assert_eq!(a.text, "One two three. Quatre cinq. Seven eight nine. Ten eleven.");
        assert_eq!(&a.text[a.edit_spans[0].0..a.edit_spans[0].1], "Quatre cinq.");
        assert_eq!(&a.text[a.edit_spans[1].0..a.edit_spans[1].1], "Ten eleven.");
        assert_eq!((a.ai_words, a.total_words), (4, 10));
        // insertion between sentences keeps the original separator after it
        let e3 = edit("insert", (d.sents[0].end, d.sents[0].end), Some(" "));
        let b = assemble(p, &[&e3], &["New one."]).unwrap();
        assert_eq!(b.text, "One two three. New one. Four five six. Seven eight nine.");
        // leading/trailing whitespace in a replacement is refused
        assert!(assemble(p, &[&e1], &[" x."]).is_err());
        // a full replacement leaves no human word: refused
        let all = edit("run", d.span(0, 3), None);
        assert!(assemble(p, &[&all], &["x y."]).is_err());
    }

    #[test]
    fn clause_segments_and_selection_boundaries() {
        let p = "When the long war finally ended, the tired soldiers returned home to their families. Short one.";
        let d = Doc::new(p);
        let segs = clause_segments(&d, 0);
        let got: Vec<&str> = segs.iter().map(|&(a, b)| &p[a..b]).collect();
        assert_eq!(got, vec!["When the long war finally ended,", "the tired soldiers returned home to their families."]);
        for id in ["a", "b", "c", "d", "e", "f"] {
            for pol in ["one_sentence", "scattered", "paragraph", "most", "clause", "insert", "condense", "compound"] {
                let text = "Rain fell all night. The river rose fast, and the mill road flooded. Nobody went outside today.\n\nMorning brought clear skies. Neighbours checked their fences. The school stayed closed, but the bakery opened early. Life went on quietly.";
                let d = Doc::new(text);
                if let Some(units) = select_policy(pol, &d, id) {
                    assert!(!units.is_empty());
                    let mut last_end = 0usize;
                    for (_, u) in &units {
                        let (s, e) = match *u {
                            Unit::Run { a, b } | Unit::Condense { a, b } => d.span(a, b),
                            Unit::Clause { s, e, .. } => (s, e),
                            Unit::Insert { after, .. } => (d.sents[after].end, d.sents[after].end),
                        };
                        assert!(s >= last_end, "{pol} units ordered and disjoint");
                        assert!(s == 0 || text[..s].ends_with(py_space) || s == e);
                        last_end = e;
                    }
                }
            }
        }
    }

    #[test]
    fn run_prompt_shape_and_context() {
        let (p, _) = diverse::rewrite_prompt("paraphrase", "One two three.", "", "x", None, 0, false, "");
        let out = compose_run(&p, "One two three.", &shape_sentence("paraphrase", 1, 3, 4, false, false), Some((Some("Before."), None)));
        assert!(out.ends_with("PASSAGE:\nOne two three."), "{out}");
        assert!(out.contains("a single sentence of about 3 words (no more than 4), with no line breaks."));
        assert!(out.contains("BEFORE: Before.\nAFTER: (end of text)"));
        let out2 = compose_run(&p, "One two three.", "S.", None);
        assert!(out2.ends_with(" S.\n\nOne two three."), "{out2}");
        let (f, _) = diverse::rewrite_prompt("fix_slop", "Text.", "", "x", None, 0, false, "BUNDLE");
        let out3 = compose_run(&f, "Text.", "S.", None);
        assert!(out3.ends_with("BUNDLE\n\nS.\n\nPASSAGE TO EDIT:\nText."), "{out3}");
        assert_eq!(shape_sentence("condense", 3, 20, 22, true, true), "Write your version as a shorter passage of about 20 words (no more than 22), keeping its paragraph breaks.");
        assert_eq!(run_cap("paraphrase", 10, 10), 14);
        assert_eq!(run_cap("expand", 10, 14), 20);
        assert_eq!(run_cap("condense", 4, 3), 4);
        let f = fit_text("class:lower", "same:,", "when the war ended,");
        assert_eq!(f, "start with a lowercase letter, as \"when\" does, and end with a comma (,), as \"ended,\" does (the last character of your answer must be a comma)");
    }

    #[test]
    fn policy_cycle_counts() {
        let c = policy_cycle();
        assert_eq!(c.len(), 50);
        for (p, n) in POLICIES {
            assert_eq!(c.iter().filter(|x| **x == p).count(), n);
        }
        // spread: no policy fills more than 2 consecutive slots
        for w in c.windows(3) {
            assert!(!(w[0] == w[1] && w[1] == w[2]));
        }
    }

    #[test]
    fn allocation_floor_cap_availability() {
        let e: BTreeMap<String, usize> = [("big".to_owned(), 40000), ("mid".to_owned(), 2500), ("tiny".to_owned(), 50)].into();
        let a = allocate(&e, 6000, 300, 4000, 2);
        assert_eq!(a["tiny"], 100);
        assert_eq!(a.values().sum::<usize>(), 6000);
        assert!(a["big"] <= 4000 && a["big"] > a["mid"]);
    }

    #[test]
    fn verdict_screens() {
        let parent = "One two three four five. Six seven eight nine ten.";
        let mut e = edit("run", (0, 24), None);
        e.len_lo = 2.0;
        e.len_hi = 8.0;
        let ov = Overlap::new(crate::overlap::PriorIndex::empty());
        let resp = |t: &str, fin: &str| json!({"choices":[{"message":{"content":t},"finish_reason":fin}]});
        let ok = verdict(&e, parent, &ov, 200, &resp("Uno dos tres cuatro cinco.", "stop"));
        assert_eq!(ok["accepted"], true, "{ok}");
        for (t, why) in [
            ("Sure, here it is: uno dos tres.", "meta_leading:sure"),
            ("Uno dos tres cuatro cinco", "missing_terminal_punctuation"),
            ("uno dos tres cuatro cinco.", "sentence_start_case"),
            ("\"Uno dos tres cuatro cinco.\"", "enclosing_quotes_added"),
            ("Uno dos, the passage says.", "meta_anywhere:the passage"),
            ("Uno dos tres. SECOND: cuatro.", "prompt_label:SECOND:"),
            ("Uno dos tres, let me know.", "meta_phrase:let me know"),
        ] {
            let v = verdict(&e, parent, &ov, 200, &resp(t, "stop"));
            assert!(v["reasons"].as_array().unwrap().iter().any(|r| r == why), "{t}: {v}");
        }
        let v = verdict(&e, parent, &ov, 200, &resp("Uno dos tres cuatro cinco.", "length"));
        assert!(v["reasons"].as_array().unwrap().iter().any(|r| r == "truncated_finish_length"));
        assert_eq!(verdict(&e, parent, &ov, 200, &resp("Surely uno dos tres cuatro.", "stop"))["accepted"], true);
        let mut c = edit("clause", (0, 4), None);
        c.end_rule = "same:,".into();
        c.start_rule = "class:lower".into();
        let p2 = "and so, it went on and on.";
        c.parent_span = (0, 7);
        c.context_spans = vec![(0, p2.len())];
        c.len_lo = 2.0;
        c.len_hi = 40.0;
        let v = verdict(&c, p2, &ov, 200, &resp("and therefore,", "stop"));
        assert_eq!(v["accepted"], true, "{v}");
        let v = verdict(&c, p2, &ov, 200, &resp("And therefore", "stop"));
        let r = v["reasons"].to_string();
        assert!(r.contains("clause_end_punctuation_changed") && r.contains("clause_start_class_changed"), "{r}");
    }
}
