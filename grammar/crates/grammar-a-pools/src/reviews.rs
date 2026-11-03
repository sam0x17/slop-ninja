//! Review corpus: Amazon Reviews 2023 (McAuley Lab), read from its published
//! JSONL(.gz) files.
//!
//! * **Rights.** The dataset grants no licence for the review text and asks
//!   for citation. It is admitted under the user decision of 2026-10-27 on classifier
//!   training: Train-side use, `redistribute_text:
//!   false`, one removable cohort id, terms snapshot recorded per run.
//! * **Unit.** One review body (the title is never included). A body of
//!   150..=450 whitespace words is admitted whole; a longer body yields the
//!   first run of whole lines (the reviewer's own line or paragraph breaks)
//!   that reaches 220 words without passing 450. A line with a quoted span
//!   of eight or more words (pasted blurbs, quoted passages) breaks runs and
//!   is never admitted. Nothing is cut inside a line and no prose is
//!   repaired.
//! * **Normalisation.** HTML line breaks become newlines (two or more become
//!   a paragraph break), character references are decoded, whitespace inside
//!   a line is squashed. Counts of each are kept per row and per run, so the
//!   rate of these artefacts is measurable.
//! * **Whole-review rejections.** Platform markup (Amazon video and product
//!   embeds, any other HTML tag or `[[`/`]]`), URLs,
//!   unknown character references, decoding damage, the English screen and
//!   an ASCII-letter share below 0.9.
//! * **Groups.** Unit and source family: one review. Bucket, split group and
//!   `writer_key`: an opaque hash of the dataset's user id, so no upstream
//!   account identifier is stored. Families are sampled only inside the
//!   bucket range; a reviewer outside it is never rendered.
//! * **Sampling.** Among eligible reviews (in range, dated before the cutoff,
//!   passing every screen) of one input file, the reviews with the lowest
//!   `rank(seed, unit)` are kept up to the file's quota, at most
//!   `max_per_user` per reviewer across the whole run (files in name order,
//!   reviews in rank order). Selection never depends on a model or detector.
use crate::{
    adapter::{self, Run},
    io,
    record::{self, Doc},
    text::{self, Excerpt},
    wikitext::decode_entities,
};
use anyhow::{Context, Result, ensure};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BinaryHeap, HashMap, VecDeque},
    io::BufRead,
    path::{Path, PathBuf},
    sync::Mutex,
};

pub const AMAZON_COLLECTION: &str = "amazon_reviews_2023_historical";
pub const AMAZON_TERMS: &str = "amazon-reviews-2023-citation-requested;review-text-unlicensed";
pub const AMAZON_COHORT: &str = "human_reviews_amazon_2023_v1";
pub const ADMISSION_BASIS: &str = "classifier-training use, user decision 2026-10-27";
pub const UNIT_METHOD: &str = "review_body_whole_or_first_whole_line_run_until_220_words;lines_with_quoted_span_of_8_or_more_words_break_runs;150..450_whitespace_words;html_breaks_to_newlines_two_or_more_paragraph;entities_decoded;whitespace_squashed_within_lines;original_line_and_paragraph_separators_kept_v2";
pub const SAMPLE_RULE: &str = "per input file, eligible reviews in ascending first-8-bytes-of-SHA-256(\"slop_ninja_review_sample_v1\\0\" + seed + \"\\0\" + unit key) up to the file quota; at most max_per_user per opaque reviewer key across the run, files visited in name order";
const USER_DOMAIN: &str = "slop_ninja_review_user_v1\0";
const UNIT_DOMAIN: &str = "slop_ninja_review_unit_v1\0";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    Amazon,
}

impl Source {
    fn name(self) -> &'static str {
        match self {
            Source::Amazon => "amazon_reviews_2023",
        }
    }
}

/// Opaque grouping key for an upstream reviewer id. Recomputable from the
/// upstream id (for a removal request) but never stored with it.
pub fn user_key(src: Source, upstream_user: &str) -> String {
    let h = text::sha_hex(format!("{USER_DOMAIN}{}\0{upstream_user}", src.name()).as_bytes());
    format!("{}_user:{}", src.name(), &h[..24])
}

pub fn rank(seed: &str, unit: &str) -> u64 {
    text::h64(format!("slop_ninja_review_sample_v1\0{seed}\0{unit}").as_bytes())
}

/// One rendered line of a review body and the separator that preceded it
/// in the source (`"\n"` for a single break, `"\n\n"` for a paragraph).
#[derive(Clone, Debug, PartialEq)]
pub struct Block {
    pub text: String,
    pub sep_before: &'static str,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Artefacts {
    pub html_line_breaks: u32,
    pub entities_decoded: u32,
    pub whitespace_squashed: bool,
}

/// Render a review body into lines, or say why the whole review is refused.
pub fn render_body(raw: &str, src: Source) -> Result<(Vec<Block>, Artefacts), &'static str> {
    if raw.contains('\u{FFFD}') {
        return Err("replacement_character");
    }
    match src {
        Source::Amazon if raw.contains("[[VIDEOID:") => return Err("amazon_video_embed_markup"),
        Source::Amazon if raw.contains("[[ASIN:") => return Err("amazon_product_link_markup"),
        _ => {}
    }
    if raw.contains("[[") || raw.contains("]]") {
        return Err("bracket_markup_residue");
    }
    let mut a = Artefacts::default();
    let mut s = String::with_capacity(raw.len());
    let mut rest = raw;
    while let Some(i) = rest.find('<') {
        s.push_str(&rest[..i]);
        let tail = &rest[i..];
        if let Some(n) = html_break_len(tail) {
            s.push('\n');
            a.html_line_breaks += 1;
            rest = &tail[n..];
            continue;
        }
        let next = tail[1..].chars().next();
        if next.is_some_and(|c| c.is_ascii_alphabetic() || c == '/' || c == '!' || c == '?') {
            return Err("html_tag_residue");
        }
        s.push('<');
        rest = &tail[1..];
    }
    s.push_str(rest);
    let s = s.replace("\r\n", "\n").replace('\r', "\n");
    let decoded = decode_entities(&s);
    if decoded.contains(text::GAP) {
        return Err("unknown_character_reference");
    }
    a.entities_decoded = count_references(&s);
    let mut blocks = Vec::new();
    let mut blank = false;
    for line in decoded.split('\n') {
        let sq = text::squash(line);
        if sq.is_empty() {
            blank = true;
            continue;
        }
        if sq != line {
            a.whitespace_squashed = true;
        }
        let sep = if blocks.is_empty() || !blank { "\n" } else { "\n\n" };
        blocks.push(Block { text: sq, sep_before: sep });
        blank = false;
    }
    Ok((blocks, a))
}

/// Character references that `decode_entities` replaces with a character.
fn count_references(s: &str) -> u32 {
    let mut n = 0;
    for (i, _) in s.match_indices('&') {
        let tail = &s[i + 1..];
        if let Some((k, _)) = tail.char_indices().take(12).find(|&(_, c)| c == ';') {
            if k > 0 && crate::wikitext::entity(&tail[..k]).is_some() {
                n += 1;
            }
        }
    }
    n
}

/// Length of an HTML line break (`<br>`, `<br/>`, `<br />`, any case) at the start of `s`.
fn html_break_len(s: &str) -> Option<usize> {
    let b = s.as_bytes();
    if b.len() < 4 || !b[1..3].eq_ignore_ascii_case(b"br") {
        return None;
    }
    let mut i = 3;
    while i < b.len() && b[i] == b' ' {
        i += 1;
    }
    if i < b.len() && b[i] == b'/' {
        i += 1;
    }
    (i < b.len() && b[i] == b'>').then_some(i + 1)
}

fn join(blocks: &[Block]) -> String {
    let mut t = String::new();
    for (k, b) in blocks.iter().enumerate() {
        if k > 0 {
            t.push_str(b.sep_before);
        }
        t.push_str(&b.text);
    }
    t
}

/// The admitted unit of a rendered review: the whole body when it lies in
/// the bound and has no quotation line, else the first run of whole usable
/// lines that reaches the target. A line holding a quoted span of eight or
/// more words (pasted blurbs, quoted passages) is never admitted and breaks
/// runs, as quotations do for the other sources.
pub fn review_unit(blocks: &[Block]) -> Result<Excerpt, &'static str> {
    let counts: Vec<usize> = blocks.iter().map(|b| text::words(&b.text)).collect();
    let usable: Vec<bool> = blocks.iter().map(|b| !text::long_quotation(&b.text)).collect();
    let total: usize = counts.iter().sum();
    if total < text::MIN_WORDS {
        return Err("review_under_150_words");
    }
    if total <= text::MAX_WORDS && usable.iter().all(|&u| u) {
        let t = join(blocks);
        return Ok(Excerpt { words: text::words(&t), text: t, start: 0, end: blocks.len(), complete_unit: true });
    }
    let mut i = 0;
    while i < blocks.len() {
        if !usable[i] {
            i += 1;
            continue;
        }
        let (mut n, mut j) = (0, i);
        while j < blocks.len() && usable[j] {
            n += counts[j];
            j += 1;
            if n >= text::TARGET_WORDS {
                break;
            }
        }
        let run_ended = j == blocks.len() || !usable[j];
        if (text::MIN_WORDS..=text::MAX_WORDS).contains(&n) && (n >= text::TARGET_WORDS || run_ended) {
            let t = join(&blocks[i..j]);
            return Ok(Excerpt { words: text::words(&t), text: t, start: i, end: j, complete_unit: false });
        }
        // Overshoot: try the next line of the same run. Too short: skip past the run.
        i = if n > text::MAX_WORDS { i + 1 } else if run_ended { j } else { i + 1 };
    }
    Err(if usable.iter().all(|&u| u) { "no_whole_line_run_within_bounds" } else { "no_whole_line_run_within_bounds_after_quotation_breaks" })
}

/// Screens on the admitted unit itself.
pub fn screen_unit(t: &str) -> Option<&'static str> {
    if let Some(r) = text::damaged(t) {
        return Some(r);
    }
    if text::has_url(t) {
        return Some("url");
    }
    if let Some(r) = text::english_screen(t) {
        return Some(r);
    }
    if text::ascii_letter_ratio(t) < 0.9 {
        return Some("non_ascii_letters");
    }
    None
}

/// Platform or genre conventions that could act as an easy "human" cue.
/// Flagged per row and counted, never used to reject.
pub fn convention_flags(t: &str) -> Vec<&'static str> {
    let l = t.to_lowercase();
    let mut f = Vec::new();
    if (l.contains("in exchange for") && (l.contains("honest") || l.contains("unbiased")))
        || ((l.contains("received this") || l.contains("was provided") || l.contains("was given")) && (l.contains("free") || l.contains("discount")))
    {
        f.push("incentive_disclosure");
    }
    if l.contains("vine") {
        f.push("mentions_vine");
    }
    if l.lines().any(|x| {
        let x = x.trim_start();
        x.starts_with("edit:") || x.starts_with("update:") || x.starts_with("edit -") || x.starts_with("update -") || x.starts_with("**update") || x.starts_with("**edit")
    }) {
        f.push("edit_or_update_note");
    }
    if l.contains("spoiler") {
        f.push("mentions_spoiler");
    }
    if t.chars().any(|c| (c as u32) >= 0x1F300) {
        f.push("emoji");
    }
    f
}

// ---- dates ----

/// Days since 1970-01-01 of a proleptic Gregorian date.
pub fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m as i64 + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

pub fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

pub fn iso_from_unix_seconds(s: i64) -> String {
    let (days, secs) = (s.div_euclid(86_400), s.rem_euclid(86_400));
    let (y, m, d) = civil_from_days(days);
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", secs / 3600, secs % 3600 / 60, secs % 60)
}

// ---- upstream rows ----

#[derive(Deserialize)]
struct AmazonRow {
    #[serde(default)]
    text: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    user_id: String,
    #[serde(default)]
    asin: String,
    #[serde(default)]
    parent_asin: String,
    timestamp: Option<i64>,
    rating: Option<f64>,
    helpful_vote: Option<i64>,
    verified_purchase: Option<bool>,
}

/// A screened, eligible review held in the per-file reservoir.
struct Pending {
    rank: u64,
    unit: String,
    user: String,
    excerpt: Excerpt,
    artefacts: Artefacts,
    blocks: usize,
    flags: Vec<&'static str>,
    date: String,
    provenance: Value,
    unit_meta: Value,
}

impl PartialEq for Pending {
    fn eq(&self, o: &Self) -> bool {
        (self.rank, &self.unit) == (o.rank, &o.unit)
    }
}
impl Eq for Pending {}
impl PartialOrd for Pending {
    fn partial_cmp(&self, o: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(o))
    }
}
impl Ord for Pending {
    fn cmp(&self, o: &Self) -> std::cmp::Ordering {
        (self.rank, &self.unit).cmp(&(o.rank, &o.unit))
    }
}

#[derive(Default)]
struct FileOutcome {
    file: String,
    label: String,
    sha256: String,
    bytes: u64,
    lines: u64,
    counts: BTreeMap<String, u64>,
    eligible: u64,
    eligible_by_year: BTreeMap<String, u64>,
    eligible_artefacts: BTreeMap<&'static str, u64>,
    pending: Vec<Pending>,
}

impl FileOutcome {
    fn count(&mut self, r: &str) {
        *self.counts.entry(r.to_owned()).or_default() += 1;
    }
}

pub struct Options<'a> {
    pub source: Source,
    pub inputs: &'a [PathBuf],
    pub seed: &'a str,
    pub quotas: Vec<u64>,
    pub max_per_user: u64,
    pub buckets: Option<(u32, u32)>,
    pub threads: usize,
    pub terms_evidence: &'a [PathBuf],
}

/// Reviews prepared from one upstream row, before sampling.
struct Prepared {
    unit: String,
    user: String,
    raw_text: String,
    date: String,
    provenance: Value,
    unit_meta: Value,
}

fn prepare_amazon(line: &str, label: &str, line_no: u64, out: &mut FileOutcome) -> Option<Prepared> {
    let Ok(r) = serde_json::from_str::<AmazonRow>(line) else {
        out.count("json_parse_error");
        return None;
    };
    if r.user_id.is_empty() {
        out.count("no_user_id");
        return None;
    }
    let Some(ts) = r.timestamp else {
        out.count("no_timestamp");
        return None;
    };
    let user = user_key(Source::Amazon, &r.user_id);
    let h = text::sha_hex(format!("{UNIT_DOMAIN}amazon_reviews_2023\0{label}\0{line_no}\0{}\0{}\0{}\0{ts}", r.user_id, r.parent_asin, r.asin).as_bytes());
    let unit = format!("amazon_reviews_2023_review:{}", &h[..32]);
    let date = iso_from_unix_seconds(ts.div_euclid(1000));
    Some(Prepared {
        provenance: json!({"dataset": "McAuley-Lab/Amazon-Reviews-2023", "category": label, "line": line_no, "asin": r.asin, "parent_asin": r.parent_asin,
                           "timestamp_ms": ts, "rating": r.rating, "helpful_vote": r.helpful_vote, "verified_purchase": r.verified_purchase,
                           "title_sha256": text::sha_hex(r.title.as_bytes()), "reviewer_key": user,
                           "upstream_user_id_stored": false}),
        unit_meta: json!({"kind": "amazon_review_body", "category": label, "line": line_no}),
        unit,
        user,
        raw_text: r.text,
        date,
    })
}

fn scan_file(o: &Options, path: &Path, quota: u64) -> Result<FileOutcome> {
    let label = path.file_name().and_then(|n| n.to_str()).unwrap_or("").split('.').next().unwrap_or("").to_owned();
    let mut out = FileOutcome { file: path.file_name().and_then(|n| n.to_str()).unwrap_or("").to_owned(), label: label.clone(), ..Default::default() };
    let f = std::fs::File::open(path).with_context(|| format!("open {}", path.display()))?;
    let mut hr = io::HashingReader::new(f);
    let cap = (quota + (quota / 4).max(50)) as usize;
    let mut heap: BinaryHeap<Pending> = BinaryHeap::new();
    {
        let reader: Box<dyn std::io::Read + '_> = if path.extension().is_some_and(|e| e == "gz") {
            Box::new(flate2::read::MultiGzDecoder::new(std::io::BufReader::with_capacity(1 << 20, &mut hr)))
        } else {
            Box::new(&mut hr)
        };
        let mut br = std::io::BufReader::with_capacity(1 << 20, reader);
        let mut line = String::new();
        loop {
            line.clear();
            if br.read_line(&mut line).with_context(|| format!("read {}", path.display()))? == 0 {
                break;
            }
            if line.trim().is_empty() {
                continue;
            }
            out.lines += 1;
            let n = out.lines;
            let p = match o.source {
                Source::Amazon => prepare_amazon(&line, &label, n, &mut out),
            };
            let Some(p) = p else { continue };
            if let Some((a, b)) = o.buckets {
                let k = adapter::bucket(&p.user);
                if !(a <= k && k < b) {
                    out.count("contributor_bucket_outside_run_range");
                    continue;
                }
            }
            if !record::before_cutoff(&p.date) {
                out.count("not_dated_before_2022_11_30");
                continue;
            }
            if p.raw_text.len() < 2 * text::MIN_WORDS - 1 {
                out.count("review_under_150_words");
                continue;
            }
            let (blocks, art) = match render_body(&p.raw_text, o.source) {
                Ok(x) => x,
                Err(r) => {
                    out.count(&format!("screen:{r}"));
                    continue;
                }
            };
            let ex = match review_unit(&blocks) {
                Ok(e) => e,
                Err(r) => {
                    out.count(r);
                    continue;
                }
            };
            if let Some(r) = screen_unit(&ex.text) {
                out.count(&format!("screen:{r}"));
                continue;
            }
            out.eligible += 1;
            *out.eligible_by_year.entry(p.date[..4].to_owned()).or_default() += 1;
            for (k, on) in [("html_line_breaks", art.html_line_breaks > 0), ("entities_decoded", art.entities_decoded > 0), ("whitespace_squashed", art.whitespace_squashed), ("partial_unit", !ex.complete_unit)] {
                if on {
                    *out.eligible_artefacts.entry(k).or_default() += 1;
                }
            }
            let flags = convention_flags(&ex.text);
            for f in &flags {
                *out.eligible_artefacts.entry(f).or_default() += 1;
            }
            let rk = rank(o.seed, &p.unit);
            if heap.len() >= cap && heap.peek().is_some_and(|m| rk > m.rank) {
                continue;
            }
            heap.push(Pending { rank: rk, unit: p.unit, user: p.user, artefacts: art, blocks: blocks.len(), flags, date: p.date, provenance: p.provenance, unit_meta: p.unit_meta, excerpt: ex });
            if heap.len() > cap {
                heap.pop();
            }
        }
    }
    let (sha, bytes) = hr.finish();
    out.sha256 = sha;
    out.bytes = bytes;
    out.pending = heap.into_sorted_vec();
    Ok(out)
}

/// Quotas proportional to the square root of each input's size, with a floor.
pub fn sqrt_quotas(sizes: &[u64], total: u64, floor: u64) -> Vec<u64> {
    let roots: Vec<f64> = sizes.iter().map(|&b| (b as f64).sqrt()).collect();
    let sum: f64 = roots.iter().sum();
    roots.iter().map(|r| if sum > 0.0 { ((total as f64) * r / sum).round() as u64 } else { 0 }.max(floor)).collect()
}

fn rights(src: Source, label: &str) -> Value {
    match src {
        Source::Amazon => json!({
            "license": AMAZON_TERMS,
            "license_url": null,
            "terms_url": "https://amazon-reviews-2023.github.io/",
            "terms_summary": "The dataset site publishes no licence and asks for citation of Hou et al. (2024). Rights in the review text remain with its owners; the dataset does not address the platform's own terms.",
            "citation": "Yupeng Hou, Jiacheng Li, Zhankui He, An Yan, Xiusi Chen, Julian McAuley. Bridging Language and Items for Retrieval and Recommendation. arXiv:2403.03952 (2024).",
            "attribution": format!("Amazon Reviews 2023 (McAuley Lab, UC San Diego), category {label}; review text by an Amazon reviewer; Hou et al. 2024, arXiv:2403.03952"),
            "evidence_url": "https://amazon-reviews-2023.github.io/",
            "access": "direct ungated HTTPS download of the published review_categories JSONL.gz files; no login, agreement or technical block",
            "commercial_training": true,
            "model_release": true,
            "external_evaluation": false,
            "redistribute_text": false,
            "use_scope": "train_and_train_mining_only",
            "cohort_id": AMAZON_COHORT,
            "admission_basis": ADMISSION_BASIS,
            "admission_decision": "user_2026-10-27: review corpora admitted for A's classifier training; lawful access, per-row terms recorded, text kept in ignored storage and never redistributed, one removable cohort per source, opaque reviewer keys",
            "notes": "Train-side only. Remove the whole cohort by cohort_id if a rights holder objects.",
        }),
    }
}

/// Scan every input (in parallel), then select and write candidates.
pub fn adapt(o: &Options, run: &mut Run) -> Result<Value> {
    ensure!(!o.inputs.is_empty(), "no inputs");
    ensure!(o.quotas.len() == o.inputs.len(), "one quota per input");
    ensure!(o.max_per_user >= 1, "max_per_user must be at least 1");
    let mut order: Vec<usize> = (0..o.inputs.len()).collect();
    order.sort_by(|&a, &b| o.inputs[a].file_name().cmp(&o.inputs[b].file_name()));
    let queue: Mutex<VecDeque<usize>> = Mutex::new(order.iter().copied().collect());
    let results: Mutex<Vec<Option<Result<FileOutcome>>>> = Mutex::new((0..o.inputs.len()).map(|_| None).collect());
    std::thread::scope(|s| {
        for _ in 0..o.threads.max(1) {
            s.spawn(|| {
                loop {
                    let Some(i) = queue.lock().unwrap().pop_front() else { break };
                    let r = scan_file(o, &o.inputs[i], o.quotas[i]);
                    results.lock().unwrap()[i] = Some(r);
                }
            });
        }
    });
    let mut outcomes = Vec::new();
    for (i, r) in results.into_inner().unwrap().into_iter().enumerate() {
        outcomes.push(r.with_context(|| format!("input {i} not scanned"))??);
    }
    let (collection, domain, method) = match o.source {
        Source::Amazon => (AMAZON_COLLECTION, "reviews_consumer_products", format!("amazon_reviews_2023_jsonl;{UNIT_METHOD}")),
    };
    let mut per_user: HashMap<String, u64> = HashMap::new();
    let mut files = Vec::new();
    let mut selected_artefacts: BTreeMap<&'static str, u64> = BTreeMap::new();
    let mut selected_by_year: BTreeMap<String, u64> = BTreeMap::new();
    let mut total_eligible = 0u64;
    let mut total_selected = 0u64;
    for &i in &order {
        let oc = std::mem::take(&mut outcomes[i]);
        run.units_seen += oc.lines;
        for (k, v) in &oc.counts {
            run.count_n(k, *v);
        }
        run.input(json!({"role": "review_jsonl", "path": o.inputs[i], "sha256": oc.sha256, "bytes": oc.bytes, "lines": oc.lines,
                         "hash_basis": "SHA-256 of the file bytes as stored (compressed)"}));
        let quota = o.quotas[i];
        let (mut sel, mut capped, mut spare) = (0u64, 0u64, 0u64);
        for p in oc.pending {
            if sel >= quota {
                spare += 1;
                continue;
            }
            let c = per_user.entry(p.user.clone()).or_default();
            if *c >= o.max_per_user {
                capped += 1;
                continue;
            }
            let mut unit = p.unit_meta;
            unit["span_basis"] = json!("review_body_line_index_half_open");
            unit["lines"] = json!(p.blocks);
            unit["artefacts"] = json!({"html_line_breaks_normalised": p.artefacts.html_line_breaks, "character_references_decoded": p.artefacts.entities_decoded,
                                       "whitespace_squashed_within_lines": p.artefacts.whitespace_squashed});
            unit["convention_flags"] = json!(p.flags);
            unit["sample_rank"] = json!(format!("{:016x}", p.rank));
            let mut prov = p.provenance;
            prov["cohort_id"] = json!(match o.source { Source::Amazon => AMAZON_COHORT });
            let id = format!("pool-{}-{}", match o.source { Source::Amazon => "amazon23" }, p.unit.rsplit(':').next().unwrap_or(""));
            let doc = Doc {
                id,
                collection,
                domain,
                source_group: p.unit.clone(),
                writer_key: Some(p.user.clone()),
                evidence: "historical_proxy",
                excerpt: &p.excerpt,
                extraction_method: &method,
                unit,
                provenance: prov,
                date: Some(p.date.clone()),
                date_basis: match o.source { Source::Amazon => "amazon_review_timestamp_utc" },
                cutoff_policy: "dated_before_cutoff",
                rights: rights(o.source, &oc.label),
                aliases: vec![],
            };
            if run.accept(&p.unit, &doc)? {
                sel += 1;
                *c += 1;
                *selected_by_year.entry(p.date[..4].to_owned()).or_default() += 1;
                for (k, on) in [("html_line_breaks", p.artefacts.html_line_breaks > 0), ("entities_decoded", p.artefacts.entities_decoded > 0), ("whitespace_squashed", p.artefacts.whitespace_squashed), ("partial_unit", !p.excerpt.complete_unit)] {
                    if on {
                        *selected_artefacts.entry(k).or_default() += 1;
                    }
                }
                for f in &p.flags {
                    *selected_artefacts.entry(f).or_default() += 1;
                }
            }
        }
        run.count_n("eligible_not_selected_above_quota", spare + (oc.eligible.saturating_sub(sel + capped + spare)));
        run.count_n("per_user_cap", capped);
        total_eligible += oc.eligible;
        total_selected += sel;
        files.push(json!({"file": oc.file, "label": oc.label, "lines": oc.lines, "eligible": oc.eligible, "quota": quota, "selected": sel,
                          "shortfall": quota.saturating_sub(sel), "per_user_cap_skips": capped, "counts": oc.counts,
                          "eligible_by_year": oc.eligible_by_year, "eligible_artefact_and_flag_counts": oc.eligible_artefacts}));
    }
    let mut terms = Vec::new();
    for t in o.terms_evidence {
        let (sha, bytes) = io::file_sha(t)?;
        terms.push(json!({"path": t, "sha256": sha, "bytes": bytes}));
    }
    let rate = |m: &BTreeMap<&'static str, u64>, n: u64| -> Value {
        m.iter().map(|(k, v)| ((*k).to_owned(), json!({"count": v, "rate": if n > 0 { *v as f64 / n as f64 } else { 0.0 }}))).collect::<serde_json::Map<_, _>>().into()
    };
    Ok(json!({
        "source": o.source.name(),
        "cohort_id": match o.source { Source::Amazon => AMAZON_COHORT },
        "admission_basis": ADMISSION_BASIS,
        "terms_evidence": terms,
        "unit_method": UNIT_METHOD,
        "sample_rule": SAMPLE_RULE,
        "seed": o.seed,
        "max_per_user": o.max_per_user,
        "eligible": total_eligible,
        "selected": total_selected,
        "files": files,
        "selected_by_year": selected_by_year,
        "selected_artefact_and_flag_rates": rate(&selected_artefacts, total_selected),
        "grouping": "unit and source family: one review; bucket, split group and writer_key: opaque hash of the dataset user id",
        "limits": [
            "review timestamps are the posting times recorded by the dataset; later edits on the platform are not visible",
            "pre-2022 reviews may include paid, incentivised or template-written reviews; only disclosure phrases are flagged",
            "reviewer keys separate dataset user ids, not people with several accounts",
            "the English screen is a function-word heuristic"
        ],
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(n: usize, w: &str) -> String {
        vec![w; n].join(" ")
    }

    #[test]
    fn breaks_entities_and_separators_are_normalised() {
        let raw = "First line here.<br />Second  line &#34;quoted&#34; here.<br /><br />New paragraph &amp; more.";
        let (b, a) = render_body(raw, Source::Amazon).unwrap();
        assert_eq!(b.len(), 3);
        assert_eq!(b[1].text, "Second line \"quoted\" here.");
        assert_eq!(b[1].sep_before, "\n");
        assert_eq!(b[2].sep_before, "\n\n");
        assert_eq!(a.html_line_breaks, 3);
        assert_eq!(a.entities_decoded, 3);
        assert!(a.whitespace_squashed);
        assert_eq!(join(&b), "First line here.\nSecond line \"quoted\" here.\n\nNew paragraph & more.");
        // A literal "<3" is text, not markup.
        assert!(render_body("I love it <3 a lot.", Source::Amazon).is_ok());
    }

    #[test]
    fn platform_markup_rejects_the_whole_review() {
        assert_eq!(render_body("[[VIDEOID:abc]] Nice product.", Source::Amazon).unwrap_err(), "amazon_video_embed_markup");
        assert_eq!(render_body("See [[ASIN:B000]] too.", Source::Amazon).unwrap_err(), "amazon_product_link_markup");
        assert_eq!(render_body("Read <a href=\"x\">this</a> now.", Source::Amazon).unwrap_err(), "html_tag_residue");
        assert_eq!(render_body("Odd &zzqq; entity.", Source::Amazon).unwrap_err(), "unknown_character_reference");
    }

    #[test]
    fn unit_is_whole_body_or_first_whole_line_run() {
        let short = vec![Block { text: words(100, "alpha"), sep_before: "\n" }];
        assert_eq!(review_unit(&short).unwrap_err(), "review_under_150_words");
        let whole = vec![Block { text: words(100, "alpha"), sep_before: "\n" }, Block { text: words(100, "beta"), sep_before: "\n\n" }];
        let e = review_unit(&whole).unwrap();
        assert!(e.complete_unit);
        assert_eq!(e.words, 200);
        assert!(e.text.contains("alpha\n\nbeta"));
        let long = vec![
            Block { text: words(500, "gamma"), sep_before: "\n" },
            Block { text: words(120, "delta"), sep_before: "\n" },
            Block { text: words(120, "eps"), sep_before: "\n" },
            Block { text: words(50, "zeta"), sep_before: "\n" },
        ];
        let e = review_unit(&long).unwrap();
        assert!(!e.complete_unit);
        assert_eq!((e.start, e.end, e.words), (1, 3, 240));
        let quoted = vec![
            Block { text: words(100, "theta"), sep_before: "\n" },
            Block { text: format!("\"{}\"", words(60, "blurb")), sep_before: "\n" },
            Block { text: words(230, "iota"), sep_before: "\n" },
        ];
        let e = review_unit(&quoted).unwrap();
        assert_eq!((e.start, e.end, e.complete_unit), (2, 3, false));
        assert!(!e.text.contains("blurb"));
        let only_short = vec![Block { text: words(100, "kappa"), sep_before: "\n" }, Block { text: format!("\"{}\"", words(60, "q")), sep_before: "\n" }];
        assert_eq!(review_unit(&only_short).unwrap_err(), "no_whole_line_run_within_bounds_after_quotation_breaks");
        let unsplittable = vec![Block { text: words(600, "eta"), sep_before: "\n" }];
        assert_eq!(review_unit(&unsplittable).unwrap_err(), "no_whole_line_run_within_bounds");
    }

    #[test]
    fn screens_and_flags() {
        let en = "I bought this kettle for my kitchen and it has been with me for a year. It is easy to use and the handle does not get hot. ";
        assert_eq!(screen_unit(&en.repeat(6)), None);
        assert_eq!(screen_unit(&format!("{} see https://example.com", en.repeat(6))), Some("url"));
        assert_eq!(screen_unit(&words(160, "producto")), Some("english_stopword_share_below_0_20"));
        let f = convention_flags("I received this product at a discount in exchange for my honest review.\nUpdate: still works.");
        assert_eq!(f, vec!["incentive_disclosure", "edit_or_update_note"]);
    }

    #[test]
    fn dates_convert_exactly() {
        assert_eq!(iso_from_unix_seconds(0), "1970-01-01T00:00:00Z");
        assert_eq!(iso_from_unix_seconds(1_669_766_400), "2022-11-30T00:00:00Z");
        assert_eq!(iso_from_unix_seconds(951_782_400), "2000-02-29T00:00:00Z");
        assert_eq!(civil_from_days(days_from_civil(2016, 12, 31)), (2016, 12, 31));
    }

    #[test]
    fn user_keys_are_opaque_and_stable() {
        let k = user_key(Source::Amazon, "AXYZUSER");
        assert!(k.starts_with("amazon_reviews_2023_user:") && !k.contains("AXYZUSER"));
        assert_eq!(k, user_key(Source::Amazon, "AXYZUSER"));
        assert_eq!(sqrt_quotas(&[100, 400, 0], 30, 2), vec![10, 20, 2]);
    }

    fn amazon_line(user: &str, ts_ms: i64, body: &str) -> String {
        json!({"rating": 5.0, "title": "t", "text": body, "images": [], "asin": "B1", "parent_asin": "B1", "user_id": user,
               "timestamp": ts_ms, "helpful_vote": 0, "verified_purchase": true}).to_string()
    }

    #[test]
    fn end_to_end_sampling_cap_and_cutoff() {
        let d = tempfile::tempdir().unwrap();
        let body = "We used this tent on four weekend trips and it kept us dry every time. The poles are light and the zips have not snagged once. ".repeat(6);
        let mut lines = Vec::new();
        for u in 0..6 {
            for k in 0..3 {
                lines.push(amazon_line(&format!("U{u}"), 1_500_000_000_000 + k, &format!("{body} Trip note {u} {k}.")));
            }
        }
        lines.push(amazon_line("LATE", 1_700_000_000_000, &body));
        lines.push(amazon_line("SHORT", 1_500_000_000_000, "Too short."));
        lines.push("{not json".into());
        let input = d.path().join("Sports.jsonl");
        std::fs::write(&input, lines.join("\n") + "\n").unwrap();
        let out = d.path().join("out");
        let mut run = Run::start_sampled(&out, "amazon-reviews-2023", AMAZON_COLLECTION, json!({}), None, None).unwrap();
        let inputs = vec![input];
        let o = Options { source: Source::Amazon, inputs: &inputs, seed: "s", quotas: vec![8], max_per_user: 1, buckets: None, threads: 2, terms_evidence: &[] };
        let extra = adapt(&o, &mut run).unwrap();
        assert_eq!(extra["eligible"], 18);
        assert_eq!(extra["selected"], 6, "one per reviewer");
        let rep = run.finish(extra).unwrap();
        assert_eq!(rep["counted_without_ledger_rows"]["not_dated_before_2022_11_30"], 1);
        assert_eq!(rep["counted_without_ledger_rows"]["review_under_150_words"], 1);
        assert_eq!(rep["counted_without_ledger_rows"]["json_parse_error"], 1);
        let rows: Vec<Value> = std::fs::read_to_string(out.join("candidates.jsonl")).unwrap().lines().map(|l| serde_json::from_str(l).unwrap()).collect();
        assert_eq!(rows.len(), 6);
        for r in &rows {
            assert_eq!(r["rights"]["redistribute_text"], false);
            assert_eq!(r["rights"]["cohort_id"], AMAZON_COHORT);
            assert!(!r.to_string().contains("\"U1\""), "upstream user ids are never stored");
            assert!(r["date"]["value"].as_str().unwrap() < "2022-11-30");
        }
        // Buckets: a range that excludes every reviewer renders nothing.
        let out2 = d.path().join("out2");
        let mut run2 = Run::start_sampled(&out2, "amazon-reviews-2023", AMAZON_COLLECTION, json!({}), None, None).unwrap();
        let keep: Vec<u32> = (0..6).map(|u| adapter::bucket(&user_key(Source::Amazon, &format!("U{u}")))).collect();
        let free = (0..1000).find(|b| !keep.contains(b)).unwrap();
        let o2 = Options { buckets: Some((free, free + 1)), quotas: vec![8], ..o };
        let e2 = adapt(&o2, &mut run2).unwrap();
        assert_eq!(e2["selected"], 0);
    }
}
