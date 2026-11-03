//! Student and learner essay corpora released as CSV tables: PERSUADE 2.0,
//! ELLIPSE and ASAP 2.0 (Learning Agency Lab, Vanderbilt and Georgia State
//! University; CC BY 4.0).
//!
//! * **Rights.** CC BY 4.0 as stated in each repository README at the pinned
//!   commit (earlier README revisions stated CC BY-NC-SA 4.0; both are
//!   admissible under the 2026-10-27 data-rights decision on classifier
//!   training). Text stays in ignored storage and is never
//!   redistributed (`redistribute_text: false`).
//! * **Dates.** The releases carry no per-essay dates. PERSUADE and ELLIPSE
//!   are dated by a documented corpus terminus ante quem (their use as the
//!   train and hidden test sets of public competitions that opened before
//!   2022-11-30). ASAP 2.0 essays that are not PERSUADE essays have no such
//!   evidence and are rejected unless a terminus and its basis are supplied.
//! * **Overlap.** ASAP 2.0 includes PERSUADE essays and ELLIPSE shares some
//!   essays with PERSUADE. Given the PERSUADE run's text-free id list, those
//!   rows are rejected by Kaggle essay id so the PERSUADE copy is the only
//!   one; text-level duplicates are left to `dedup`.
//! * **Groups.** Unit and source family: one essay
//!   (`<corpus>:essay:<id>`). Bucket and split group: the writing prompt
//!   (`student_essay_prompt:<slug>`), shared across the three corpora, so a
//!   prompt never spans splits. The releases have no writer identifier, so
//!   writer closure is unavailable (recorded limit).
//! * **Normalisation (recorded per row, never prose repair).** CRLF and
//!   lone CR become LF; C1 control characters are decoded as Windows-1252
//!   (the typed apostrophes and dashes they encode); whitespace inside a
//!   paragraph, including no-break spaces and doubled spaces, is collapsed
//!   to one space so typing artefacts cannot become a human cue.
//! * **Paragraphs.** Blank lines separate paragraphs (single line breaks
//!   when an essay has no blank line). A paragraph breaks the run when it
//!   carries a de-identification placeholder (`Generic_Name`,
//!   `STUDENT_NAME`, any `word_word` token), mojibake, a URL, damage, a quoted
//!   span of 8+ words, or when at least half of its folded tokens lie in
//!   10-token windows shared by 5+ essays of the same prompt (copied source
//!   or prompt text). There is no sentence-end or minimum-length rule, so
//!   weak and unpunctuated student prose is kept. The shared paragraph run
//!   then applies (`text::paragraph_run`), followed by the English screen
//!   and an excerpt-level shared-window cap.
use crate::{
    adapter::{self, Run},
    csvread::Table,
    io::{self, JsonlWriter},
    record::{self, Doc},
    text::{self, Para},
};
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Map, Value, json};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    io::Read,
    path::{Path, PathBuf},
};

pub const DOMAIN: &str = "student_essay";
pub const LICENSE: &str = "CC-BY-4.0";
/// Folded-token window for the copied-text measure.
pub const WINDOW: usize = 10;
/// A window is common when this many distinct essays of one prompt share it.
pub const COMMON_MIN_ESSAYS: u32 = 5;
/// A paragraph with at least this share of tokens in common windows breaks the run.
pub const PARA_SHARED_BREAK: f64 = 0.5;
/// An excerpt with more than this share of tokens in common windows is rejected.
pub const EXCERPT_SHARED_MAX: f64 = 0.3;
pub const METHOD: &str = "student_essay_csv_full_text;crlf_to_lf;c1_controls_as_windows_1252;paragraphs_by_blank_line_else_line;whitespace_collapsed_within_paragraph;breaks_placeholder_mojibake_url_damage_quote8_shared_window10x5_half_v1";

pub struct Spec {
    pub corpus: &'static str,
    pub collection: &'static str,
    pub title: &'static str,
    pub repository: &'static str,
    pub commit: &'static str,
    pub id_col: &'static str,
    pub prompt_col: &'static str,
    pub text_col: &'static str,
    /// (output key, column) for writer demographics, kept for slice reporting.
    pub demographics: &'static [(&'static str, &'static str)],
    /// Other non-prose columns kept as provenance.
    pub fields: &'static [&'static str],
    pub terminus: Option<(&'static str, &'static str)>,
    pub attribution: &'static str,
    pub licence_history: &'static str,
    pub exposure: &'static [&'static str],
    /// ELL status fixed by corpus definition (ELLIPSE writers are all ELLs).
    pub ell_by_definition: Option<&'static str>,
}

pub const PERSUADE: Spec = Spec {
    corpus: "persuade2",
    collection: "persuade_2_0_student_essays",
    title: "PERSUADE 2.0",
    repository: "https://github.com/scrosseye/persuade_corpus_2.0",
    commit: "5bdbad1175d1d5adec082b3f1c4e3ce37b7bd1bf",
    id_col: "essay_id_comp",
    prompt_col: "prompt_name",
    text_col: "full_text",
    demographics: &[
        ("grade", "grade_level"),
        ("ell_status", "ell_status"),
        ("gender", "gender"),
        ("race_ethnicity", "race_ethnicity"),
        ("economic_status", "economically_disadvantaged"),
        ("disability_status", "student_disability_status"),
    ],
    fields: &["essay_id", "competition_set", "provider", "task", "holistic_essay_score", "essay_word_count"],
    terminus: Some((
        "2022-03-25",
        "corpus_terminus_ante_quem: all PERSUADE essays (15,594 train, 10,402 hidden test) were the data of the Kaggle Feedback Prize - Evaluating Student Writing competition (winter 2021-2022), and the public PERSUADE 1.0 repository was created 2022-03-25; the release publishes no per-essay composition date",
    )),
    attribution: "Crossley, S. A., Baffour, P., Tian, Y., Franklin, A., Benner, M., & Boser, U. (2024). A large-scale corpus for assessing written argumentation: PERSUADE 2.0. Assessing Writing, 61. https://github.com/scrosseye/persuade_corpus_2.0 (CC BY 4.0)",
    licence_history: "README at commit 5bdbad11 (2026-10-27) states CC BY 4.0; the previous README revision stated CC BY-NC-SA 4.0. Data CSVs are the public Google Drive files linked from the README.",
    exposure: &[
        "Kaggle Feedback Prize - Evaluating Student Writing (2021-2022) and Predicting Effective Arguments (2022)",
        "human-written side of the Kaggle LLM - Detect AI Generated Text competition (2023-2024): public detector training exposure is likely",
    ],
    ell_by_definition: None,
};

pub const ELLIPSE: Spec = Spec {
    corpus: "ellipse",
    collection: "ellipse_ell_student_essays",
    title: "ELLIPSE",
    repository: "https://github.com/scrosseye/ELLIPSE-Corpus",
    commit: "710963bbf1acf23a28e40643d9f937d06f047dd4",
    id_col: "text_id_kaggle",
    prompt_col: "prompt",
    text_col: "full_text",
    demographics: &[
        ("grade", "grade"),
        ("gender", "gender"),
        ("race_ethnicity", "race_ethnicity"),
        ("economic_status", "SES"),
    ],
    fields: &["task", "set", "Overall", "Cohesion", "Syntax", "Vocabulary", "Phraseology", "Grammar", "Conventions"],
    terminus: Some((
        "2022-08-31",
        "corpus_terminus_ante_quem: the final ELLIPSE essays (3,911 train, 2,571 test) were the public train and hidden test sets of the Kaggle Feedback Prize - English Language Learning competition, which opened 2022-08-31; essays were written in state-wide standardized testing before corpus construction; the release publishes no per-essay composition date",
    )),
    attribution: "Crossley, S. A., Tian, Y., Baffour, P., Franklin, A., Kim, Y., Morris, W., Benner, B., Picou, A., & Boser, U. (2023). Measuring second language proficiency using the English Language Learner Insight, Proficiency and Skills Evaluation (ELLIPSE) Corpus. International Journal of Learner Corpus Research, 9(2). https://github.com/scrosseye/ELLIPSE-Corpus (CC BY 4.0)",
    licence_history: "README at commit 710963bb (2026-10-27) states CC BY 4.0; the previous README revision stated CC BY-NC-SA 4.0.",
    exposure: &["Kaggle Feedback Prize - English Language Learning (2022): public model training exposure is likely"],
    ell_by_definition: Some("Yes"),
};

pub const ASAP2: Spec = Spec {
    corpus: "asap2",
    collection: "asap_2_0_student_essays",
    title: "ASAP 2.0",
    repository: "https://github.com/scrosseye/ASAP_2.0",
    commit: "060ab225ca0984c0bf43ff222bf6bb2bc6195b6f",
    id_col: "essay_id",
    prompt_col: "prompt_name",
    text_col: "full_text",
    demographics: &[
        ("grade", "grade_level"),
        ("ell_status", "ell_status"),
        ("gender", "gender"),
        ("race_ethnicity", "race_ethnicity"),
        ("economic_status", "economically_disadvantaged"),
        ("disability_status", "student_disability_status"),
    ],
    fields: &["score", "set", "pubpriv", "task", "essay_word_count"],
    terminus: None,
    attribution: "Crossley, S. A., Baffour, P., Burleigh, L., & King, J. (2025). A large-scale corpus for assessing source-based writing quality: ASAP 2.0. Assessing Writing, 65. https://github.com/scrosseye/ASAP_2.0 (CC BY 4.0)",
    licence_history: "README at commit 060ab225 (2026-01-20) states CC BY 4.0; earlier README revisions stated CC BY-NC-SA 4.0.",
    exposure: &["Kaggle Learning Agency Lab - Automated Essay Scoring 2.0 (2024)"],
    ell_by_definition: None,
};

pub fn spec(corpus: &str) -> Result<&'static Spec> {
    Ok(match corpus {
        "persuade2" => &PERSUADE,
        "ellipse" => &ELLIPSE,
        "asap2" => &ASAP2,
        _ => bail!("unknown corpus {corpus} (persuade2, ellipse, asap2)"),
    })
}

/// One release table: `SPLIT=PATH`, optionally decrypted with a published password.
pub struct Input {
    pub split: String,
    pub path: PathBuf,
    pub password: Option<String>,
}

pub fn parse_inputs(inputs: &[String], passwords: &[String]) -> Result<Vec<Input>> {
    let mut pw: HashMap<String, String> = HashMap::new();
    for p in passwords {
        let (s, v) = p.split_once('=').context("--password must look like SPLIT=PASSWORD")?;
        pw.insert(s.to_owned(), v.to_owned());
    }
    let mut out = Vec::new();
    for i in inputs {
        let (s, p) = i.split_once('=').context("--input must look like SPLIT=PATH")?;
        ensure!(!s.is_empty() && !p.is_empty(), "--input must look like SPLIT=PATH");
        out.push(Input { split: s.to_owned(), path: PathBuf::from(p), password: pw.remove(s) });
    }
    ensure!(pw.is_empty(), "passwords given for unknown splits: {:?}", pw.keys().collect::<Vec<_>>());
    Ok(out)
}

/// The CSV text of an input: the file itself, or the one non-`__MACOSX`
/// `.csv` entry of a zip archive (detected by magic bytes).
pub fn read_table(i: &Input) -> Result<(String, Value)> {
    let (sha, bytes) = io::file_sha(&i.path)?;
    let mut head = [0u8; 4];
    let is_zip = std::fs::File::open(&i.path)?.read(&mut head)? == 4 && head == *b"PK\x03\x04";
    if !is_zip {
        ensure!(i.password.is_none(), "password given for a non-zip input {}", i.path.display());
        let s = String::from_utf8(std::fs::read(&i.path)?).with_context(|| format!("UTF-8 {}", i.path.display()))?;
        return Ok((s, json!({"role": "essay_table", "split": i.split, "path": i.path, "sha256": sha, "bytes": bytes, "container": "csv"})));
    }
    let mut z = zip::ZipArchive::new(std::fs::File::open(&i.path)?)?;
    let names: Vec<(usize, String)> = (0..z.len())
        .filter_map(|k| z.name_for_index(k).map(|n| (k, n.to_owned())))
        .filter(|(_, n)| n.ends_with(".csv") && !n.starts_with("__MACOSX/"))
        .collect();
    ensure!(names.len() == 1, "{} must hold exactly one CSV entry, found {}", i.path.display(), names.len());
    let (k, name) = &names[0];
    let mut f = match &i.password {
        Some(p) => z.by_index_decrypt(*k, p.as_bytes())?,
        None => z.by_index(*k)?,
    };
    let mut buf = Vec::new();
    f.read_to_end(&mut buf)?;
    let inner = text::sha_hex(&buf);
    let s = String::from_utf8(buf).with_context(|| format!("UTF-8 {}:{name}", i.path.display()))?;
    let receipt = json!({"role": "essay_table", "split": i.split, "path": i.path, "sha256": sha, "bytes": bytes, "container": "zip",
                         "entry": name, "entry_sha256": inner, "entry_bytes": s.len(), "encrypted_entry": i.password.is_some()});
    Ok((s, receipt))
}

/// `student_essay_prompt:<slug>`: lowercase ASCII alphanumeric runs joined by `_`.
pub fn prompt_key(prompt: &str) -> String {
    let mut s = String::new();
    for c in prompt.to_lowercase().chars() {
        if c.is_ascii_alphanumeric() {
            s.push(c);
        } else if !s.ends_with('_') && !s.is_empty() {
            s.push('_');
        }
    }
    format!("student_essay_prompt:{}", s.trim_end_matches('_'))
}

fn cp1252_c1(c: char) -> Option<char> {
    Some(match c as u32 {
        0x80 => '€',
        0x82 => '‚',
        0x83 => 'ƒ',
        0x84 => '„',
        0x85 => '…',
        0x86 => '†',
        0x87 => '‡',
        0x88 => 'ˆ',
        0x89 => '‰',
        0x8A => 'Š',
        0x8B => '‹',
        0x8C => 'Œ',
        0x8E => 'Ž',
        0x91 => '‘',
        0x92 => '’',
        0x93 => '“',
        0x94 => '”',
        0x95 => '•',
        0x96 => '–',
        0x97 => '—',
        0x98 => '˜',
        0x99 => '™',
        0x9A => 'š',
        0x9B => '›',
        0x9C => 'œ',
        0x9E => 'ž',
        0x9F => 'Ÿ',
        _ => return None,
    })
}

#[derive(Debug, Default, Clone, PartialEq)]
pub struct Normalised {
    pub text: String,
    pub crlf_or_cr: usize,
    pub c1_mapped: usize,
    pub nbsp: usize,
}

/// Line ends to LF and C1 controls decoded as Windows-1252; undefined C1
/// code points are kept (and later count as damage).
pub fn normalise(s: &str) -> Normalised {
    let mut n = Normalised::default();
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        if c == '\r' {
            n.crlf_or_cr += 1;
            if it.peek() == Some(&'\n') {
                it.next();
            }
            n.text.push('\n');
        } else if let Some(m) = cp1252_c1(c) {
            n.c1_mapped += 1;
            n.text.push(m);
        } else {
            if c == '\u{A0}' {
                n.nbsp += 1;
            }
            n.text.push(c);
        }
    }
    n
}

/// Paragraphs separated by blank lines, or by line breaks when the essay
/// has no blank line; each is whitespace-collapsed. Returns the basis.
pub fn split_paragraphs(t: &str) -> (Vec<String>, &'static str) {
    let lines: Vec<&str> = t.split('\n').collect();
    let blank = |l: &str| l.trim().is_empty();
    let first = lines.iter().position(|l| !blank(l));
    let last = lines.iter().rposition(|l| !blank(l));
    let inner_blank = match (first, last) {
        (Some(a), Some(b)) => lines[a..=b].iter().any(|l| blank(l)),
        _ => false,
    };
    if inner_blank {
        let mut out = Vec::new();
        let mut cur: Vec<&str> = Vec::new();
        for l in lines.iter().copied().chain(std::iter::once("")) {
            if blank(l) {
                if !cur.is_empty() {
                    out.push(text::squash(&cur.join(" ")));
                    cur.clear();
                }
            } else {
                cur.push(l);
            }
        }
        (out, "blank_line")
    } else {
        (lines.into_iter().filter(|l| !blank(l)).map(text::squash).collect(), "line")
    }
}

/// An ASCII `word_word` token: the de-identification placeholders of these
/// releases (`Generic_Name`, `STUDENT_NAME`, `PROPER_NAME`, ...).
pub fn has_placeholder(t: &str) -> bool {
    let b = t.as_bytes();
    (1..b.len().saturating_sub(1)).any(|i| b[i] == b'_' && b[i - 1].is_ascii_alphanumeric() && b[i + 1].is_ascii_alphanumeric())
}

pub fn has_mojibake(t: &str) -> bool {
    ["Ã", "Â", "â€", "ï¿½"].iter().any(|m| t.contains(m))
}

/// Why a paragraph breaks the run, before the shared-window measure.
pub fn para_reason(t: &str) -> Option<&'static str> {
    if let Some(r) = text::damaged(t) {
        return Some(r);
    }
    if has_placeholder(t) {
        return Some("deidentification_placeholder");
    }
    if has_mojibake(t) {
        return Some("mojibake");
    }
    if text::has_url(t) {
        return Some("url");
    }
    if text::long_quotation(t) {
        return Some("long_quotation");
    }
    None
}

fn window_hashes(prompt: &str, toks: &[String]) -> Vec<u64> {
    if toks.len() < WINDOW {
        return Vec::new();
    }
    (0..=toks.len() - WINDOW).map(|i| text::h64(format!("{prompt}\0{}", toks[i..i + WINDOW].join(" ")).as_bytes())).collect()
}

/// (covered tokens, tokens) of a paragraph against the common-window counts.
fn coverage(prompt: &str, t: &str, counts: &HashMap<u64, u32>) -> (usize, usize) {
    let toks = text::folded_tokens(t);
    let mut cov = vec![false; toks.len()];
    for (i, h) in window_hashes(prompt, &toks).into_iter().enumerate() {
        if counts.get(&h).is_some_and(|&n| n >= COMMON_MIN_ESSAYS) {
            cov[i..i + WINDOW].iter_mut().for_each(|c| *c = true);
        }
    }
    (cov.iter().filter(|c| **c).count(), toks.len())
}

fn clean(v: &str) -> Value {
    let t = v.trim();
    if t.is_empty() || t == "NA" { Value::Null } else { json!(t) }
}

struct Essay {
    id: String,
    split: String,
    input: usize,
    prompt: String,
    text: String,
    row: Map<String, Value>,
}

pub struct Options<'a> {
    pub spec: &'static Spec,
    pub inputs: &'a [Input],
    pub out: &'a Path,
    /// Text-free PERSUADE id list (`essay-ids.jsonl` of a PERSUADE run).
    pub persuade_ids: Option<&'a Path>,
    /// Terminus and basis for essays without built-in date evidence.
    pub collection_terminus: Option<(&'a str, &'a str)>,
}

pub fn load_persuade_ids(p: &Path) -> Result<HashSet<String>> {
    let mut s = HashSet::new();
    for r in io::jsonl_rows(p)? {
        let r = r?;
        for k in ["essay_id_comp", "essay_id"] {
            if let Some(v) = r[k].as_str().filter(|v| !v.trim().is_empty()) {
                s.insert(v.to_owned());
            }
        }
    }
    Ok(s)
}

fn bump(m: &mut BTreeMap<String, u64>, k: impl Into<String>) {
    *m.entry(k.into()).or_default() += 1;
}

pub fn adapt(o: &Options, run: &mut Run) -> Result<Value> {
    let sp = o.spec;
    ensure!(sp.terminus.is_none() || o.collection_terminus.is_none(), "{} has built-in date evidence; --collection-terminus is not accepted", sp.title);
    if let Some((d, b)) = o.collection_terminus {
        ensure!(record::valid_date(d) && !b.trim().is_empty(), "collection terminus needs YYYY-MM-DD and a basis");
    }
    let persuade_ids = match o.persuade_ids {
        Some(p) => {
            let (sha, bytes) = io::file_sha(p)?;
            let s = load_persuade_ids(p)?;
            run.input(json!({"role": "persuade_essay_ids", "path": p, "sha256": sha, "bytes": bytes, "ids": s.len()}));
            Some(s)
        }
        None => None,
    };
    ensure!(sp.corpus == "persuade2" || persuade_ids.is_some(), "--persuade-ids is required for {}", sp.title);

    // Read every table; PERSUADE repeats each essay once per discourse element.
    let mut essays: Vec<Essay> = Vec::new();
    let mut by_id: HashMap<String, usize> = HashMap::new();
    let mut inconsistent: HashSet<String> = HashSet::new();
    let mut table_rows = Vec::new();
    let mut duplicate_rows = 0u64;
    for (k, inp) in o.inputs.iter().enumerate() {
        let (s, receipt) = read_table(inp)?;
        run.input(receipt);
        let mut t = Table::new(&s)?;
        let col = |t: &Table, c: &str| t.col(c).with_context(|| format!("column {c} missing in {}", inp.path.display()));
        let (ic, pc, xc) = (col(&t, sp.id_col)?, col(&t, sp.prompt_col)?, col(&t, sp.text_col)?);
        let keep: Vec<(String, usize)> = sp.demographics.iter().map(|(_, c)| *c).chain(sp.fields.iter().copied()).filter_map(|c| t.col(c).map(|i| (c.to_owned(), i))).collect();
        let missing: Vec<&str> = sp.demographics.iter().map(|(_, c)| *c).chain(sp.fields.iter().copied()).filter(|c| t.col(c).is_none()).collect();
        ensure!(missing.is_empty(), "columns missing in {}: {missing:?}", inp.path.display());
        let mut n = 0u64;
        while let Some(r) = t.next_row()? {
            n += 1;
            let id = r[ic].trim().to_owned();
            ensure!(!id.is_empty(), "empty {} in {}", sp.id_col, inp.path.display());
            if let Some(&j) = by_id.get(&id) {
                duplicate_rows += 1;
                if essays[j].text != r[xc] || essays[j].split != inp.split {
                    inconsistent.insert(id);
                }
                continue;
            }
            let row: Map<String, Value> = keep.iter().map(|(c, i)| (c.clone(), clean(&r[*i]))).collect();
            by_id.insert(id.clone(), essays.len());
            essays.push(Essay { id, split: inp.split.clone(), input: k, prompt: r[pc].trim().to_owned(), text: r[xc].clone(), row });
        }
        table_rows.push(json!({"split": inp.split, "records": n}));
    }

    // Text-free id list (all buckets) for the other corpora's overlap gate.
    let mut ids = JsonlWriter::create(&o.out.join("essay-ids.jsonl"))?;
    for e in &essays {
        let pk = prompt_key(&e.prompt);
        ids.write(&json!({"corpus": sp.corpus, "id": e.id, "essay_id_comp": if sp.corpus == "persuade2" { json!(e.id) } else { Value::Null },
                          "essay_id": e.row.get("essay_id").cloned().unwrap_or(json!(e.id)), "prompt_key": pk, "bucket": adapter::bucket(&pk), "release_split": e.split}))?;
    }
    let (ids_sha, ids_rows) = ids.finish()?;
    let overlap_all = persuade_ids.as_ref().map(|s| essays.iter().filter(|e| s.contains(&e.id)).count());

    // Pass A: gates, paragraphs and window counts for rendered essays only.
    let mut prompts: BTreeMap<String, (u64, u64, u32)> = BTreeMap::new();
    let mut kept: Vec<(usize, Vec<String>, &'static str, Normalised)> = Vec::new();
    let mut counts: HashMap<u64, u32> = HashMap::new();
    let mut art: BTreeMap<String, u64> = BTreeMap::new();
    for (j, e) in essays.iter().enumerate() {
        if run.full() {
            break;
        }
        run.units_seen += 1;
        let pk = prompt_key(&e.prompt);
        let fam = format!("{}:essay:{}", sp.corpus, e.id);
        let unit = fam.clone();
        let entry = prompts.entry(pk.clone()).or_insert((0, 0, adapter::bucket(&pk)));
        entry.0 += 1;
        let meta = || json!({"corpus": sp.corpus, "essay_id": e.id, "prompt_name": e.prompt, "release_split": e.split,
                             "text_sha256": text::sha_hex(e.text.as_bytes()), "demographics_present": e.row.values().filter(|v| !v.is_null()).count()});
        if !run.sampled_meta_grouped(&pk, &fam, meta)? {
            continue;
        }
        entry.1 += 1;
        if inconsistent.contains(&e.id) {
            run.reject(&unit, "inconsistent_full_text_or_split_across_rows")?;
            continue;
        }
        if e.prompt.is_empty() || e.prompt == "NA" {
            run.reject(&unit, "no_prompt")?;
            continue;
        }
        if persuade_ids.as_ref().is_some_and(|s| s.contains(&e.id)) {
            run.reject(&unit, "same_essay_as_persuade_2_0_by_kaggle_id")?;
            continue;
        }
        if sp.terminus.is_none() && o.collection_terminus.is_none() {
            run.reject(&unit, "no_composition_date_evidence_before_2022_11_30")?;
            continue;
        }
        let n = normalise(&e.text);
        for (k, v) in [("essays_with_crlf_or_cr", n.crlf_or_cr), ("essays_with_c1_controls_mapped", n.c1_mapped), ("essays_with_no_break_spaces", n.nbsp)] {
            if v > 0 {
                bump(&mut art, k);
            }
        }
        let (paras, basis) = split_paragraphs(&n.text);
        if paras.is_empty() {
            run.reject(&unit, "empty_text")?;
            continue;
        }
        if has_placeholder(&n.text) {
            bump(&mut art, "essays_with_deidentification_placeholder");
        }
        if has_mojibake(&n.text) {
            bump(&mut art, "essays_with_mojibake");
        }
        if n.text.split('\n').any(|l| l.contains("  ") || l.contains('\u{A0}') || l.contains('\t')) {
            bump(&mut art, "essays_with_whitespace_runs_collapsed");
        }
        let mut seen = HashSet::new();
        for p in &paras {
            for h in window_hashes(&pk, &text::folded_tokens(p)) {
                if seen.insert(h) {
                    *counts.entry(h).or_default() += 1;
                }
            }
        }
        kept.push((j, paras, basis, n));
    }

    // Pass B: screen paragraphs, take the run and write candidates.
    let mut breaks: BTreeMap<String, u64> = BTreeMap::new();
    let mut shares: Vec<f64> = Vec::new();
    let mut slices: BTreeMap<String, u64> = BTreeMap::new();
    let mut complete_units = 0u64;
    for (j, paras, basis, n) in kept {
        let e = &essays[j];
        let pk = prompt_key(&e.prompt);
        let unit = format!("{}:essay:{}", sp.corpus, e.id);
        let mut screened = Vec::with_capacity(paras.len());
        let mut cov = Vec::with_capacity(paras.len());
        for p in paras {
            let (c, t) = coverage(&pk, &p, &counts);
            cov.push((c, t));
            let reason = para_reason(&p).or_else(|| (t > 0 && c as f64 >= PARA_SHARED_BREAK * t as f64).then_some("shared_source_or_prompt_text"));
            if let Some(r) = reason {
                bump(&mut breaks, r);
            }
            screened.push(match reason {
                Some(r) => Para::non_prose(p, r),
                None => Para { text: p, prose: true, reason: None },
            });
        }
        let Some(ex) = text::paragraph_run(&screened) else {
            run.reject(&unit, "no_paragraph_run_within_bounds")?;
            continue;
        };
        if let Some(r) = text::english_screen(&ex.text) {
            run.reject(&unit, r)?;
            continue;
        }
        let (c, t) = cov[ex.start..ex.end].iter().fold((0, 0), |a, b| (a.0 + b.0, a.1 + b.1));
        let share = if t == 0 { 0.0 } else { c as f64 / t as f64 };
        if share > EXCERPT_SHARED_MAX {
            run.reject(&unit, "excerpt_shared_window_share_above_0_30")?;
            continue;
        }
        let mut demo = Map::new();
        for (k, c) in sp.demographics {
            demo.insert((*k).to_owned(), e.row.get(*c).cloned().unwrap_or(Value::Null));
        }
        if let Some(v) = sp.ell_by_definition {
            demo.insert("ell_status".into(), json!(v));
            demo.insert("ell_status_basis".into(), json!("corpus_definition: every ELLIPSE writer is an English language learner"));
        }
        let fields: Map<String, Value> = sp.fields.iter().map(|c| ((*c).to_owned(), e.row.get(*c).cloned().unwrap_or(Value::Null))).collect();
        let (date, basis_text) = match (sp.terminus, o.collection_terminus) {
            (Some((d, b)), _) => (d, b),
            (None, Some((d, b))) => (d, b),
            _ => unreachable!("gated above"),
        };
        let inp = &o.inputs[e.input];
        let doc = Doc {
            id: format!("pool-{}-{}", sp.corpus, e.id),
            collection: sp.collection,
            domain: DOMAIN,
            source_group: unit.clone(),
            writer_key: None,
            evidence: "historical_proxy",
            excerpt: &ex,
            extraction_method: METHOD,
            unit: json!({"kind": "student_essay_full_text", "essay_id": e.id, "release_split": e.split,
                         "essay_words": text::words(&n.text), "paragraphs": screened.len(), "paragraph_basis": basis,
                         "span_basis": "essay_paragraph_index_half_open", "excerpt_shared_window_share": share,
                         "normalisation": {"crlf_or_cr": n.crlf_or_cr, "c1_controls_mapped_windows_1252": n.c1_mapped, "no_break_spaces": n.nbsp,
                                           "whitespace_collapsed_within_paragraph": true},
                         "faithful_unit_note": "whole essay is the unit; whole paragraphs only"}),
            provenance: json!({"corpus": sp.title, "repository": sp.repository, "readme_commit": sp.commit, "table": inp.path.file_name().and_then(|n| n.to_str()),
                               "release_split": e.split, "essay_id": e.id, "prompt_name": e.prompt, "prompt_key": pk,
                               "writer_demographics": demo.clone(), "fields": fields, "writer_identity": "unavailable: the release has no writer identifier",
                               "known_public_exposure": sp.exposure, "cohort_id": format!("a_comp_essays_{}_v1", sp.corpus)}),
            date: Some(date.to_owned()),
            date_basis: basis_text,
            cutoff_policy: "dated_before_cutoff",
            rights: {
                let mut r = record::rights(LICENSE, format!("{}; essay {}; whitespace normalised, whole paragraphs selected", sp.attribution, e.id), sp.repository,
                                           "Learner-essay release; writer demographics are kept as provided for slice reporting; essays are de-identified by the publisher.");
                r["admission_decision"] = json!("2026-10-27 data-rights decision: classifier training; lawfully accessible public release, licence and basis recorded per row, no redistribution, removable per-source cohort");
                r["licence_history"] = json!(sp.licence_history);
                r["redistribute_text"] = json!(false);
                r["removal_cohort"] = json!(format!("a_comp_essays_{}_v1", sp.corpus));
                r
            },
            aliases: vec![format!("kaggle_essay:{}", e.id)],
        };
        if run.accept(&unit, &doc)? {
            shares.push(share);
            if ex.complete_unit {
                complete_units += 1;
            }
            for k in ["grade", "ell_status"] {
                let v = demo.get(k).and_then(Value::as_str).unwrap_or("unknown");
                bump(&mut slices, format!("{k}:{v}"));
            }
        }
    }
    shares.sort_by(f64::total_cmp);
    let q = |p: f64| shares.get(((shares.len().saturating_sub(1)) as f64 * p) as usize).copied();
    let prompt_rows: Vec<Value> = prompts.iter().map(|(k, (n, r, b))| json!({"prompt_key": k, "essays": n, "rendered": r, "bucket": b})).collect();
    Ok(json!({
        "corpus": sp.corpus, "title": sp.title, "repository": sp.repository, "readme_commit": sp.commit,
        "tables": table_rows, "essays": essays.len(), "duplicate_rows_merged": duplicate_rows, "inconsistent_ids": inconsistent.len(),
        "essay_ids": {"file": "essay-ids.jsonl", "rows": ids_rows, "sha256": ids_sha, "content": "ids, prompt keys and buckets only; no text"},
        "persuade_id_overlap_all_buckets": overlap_all,
        "prompts": prompt_rows,
        "artefacts_in_rendered_essays": art,
        "paragraph_breaks": breaks,
        "complete_essays_admitted_whole": complete_units,
        "excerpt_shared_window_share": {"p50": q(0.5), "p90": q(0.9), "p99": q(0.99), "max": shares.last()},
        "candidate_slices": slices,
        "shared_window": {"tokens": WINDOW, "common_min_essays_per_prompt": COMMON_MIN_ESSAYS, "paragraph_break_share": PARA_SHARED_BREAK, "excerpt_max_share": EXCERPT_SHARED_MAX,
                          "counted_over": "rendered essays of the same corpus and prompt"},
        "date_rule": match (sp.terminus, o.collection_terminus) { (Some((d, b)), _) | (None, Some((d, b))) => json!({"terminus": d, "basis": b}), _ => json!("no date evidence: every non-PERSUADE essay is rejected") },
        "grouping": "unit and source family: essay; bucket and split group: prompt key shared across corpora; writer_key: none",
        "limits": ["no writer identifier: writer closure across prompts and corpora is unavailable",
                   "dates are corpus-level termini, not per-essay composition dates",
                   "copied source text is measured by cross-essay windows within the run, not against the source articles",
                   "de-identification placeholders break paragraphs; essays are publisher-de-identified, so names were already replaced"],
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn prompt_keys_and_placeholders() {
        assert_eq!(prompt_key("\"A Cowboy Who Rode the Waves\""), "student_essay_prompt:a_cowboy_who_rode_the_waves");
        assert_eq!(prompt_key("Car-free cities"), "student_essay_prompt:car_free_cities");
        assert!(has_placeholder("Dear Generic_Name, I think"));
        assert!(has_placeholder("my friend STUDENT_NAME said"));
        assert!(!has_placeholder("a _lonely_ underscore _ here"));
        assert!(has_mojibake("donÃ¢t"));
        assert_eq!(para_reason("I went to the store and it was fine with me"), None);
        assert_eq!(para_reason("see https://example.org for more about it"), Some("url"));
    }

    #[test]
    fn normalisation_and_paragraphs() {
        let n = normalise("One\r\ntwo\u{92}s\u{A0} end\rx");
        assert_eq!(n.text, "One\ntwo’s\u{A0} end\nx");
        assert_eq!((n.crlf_or_cr, n.c1_mapped, n.nbsp), (2, 1, 1));
        let (p, b) = split_paragraphs("  a  b\nc\n\n\u{A0}\n d \n\n");
        assert_eq!((p, b), (vec!["a b c".to_owned(), "d".to_owned()], "blank_line"));
        let (p, b) = split_paragraphs("one line\nsecond  line\n");
        assert_eq!((p, b), (vec!["one line".to_owned(), "second line".to_owned()], "line"));
    }

    /// Synthetic prose: half English function words, half tokens unique to the seed.
    fn para(seed: usize, n: usize) -> String {
        (0..n).map(|i| if i % 2 == 0 { "the".to_owned() } else { format!("w{seed}x{i}") }).collect::<Vec<_>>().join(" ") + "."
    }

    fn write_csv(path: &Path, rows: &[(&str, &str, &str)]) {
        let mut f = std::fs::File::create(path).unwrap();
        writeln!(f, "text_id_kaggle,full_text,gender,grade,race_ethnicity,SES,task,set,Overall,Cohesion,Syntax,Vocabulary,Phraseology,Grammar,Conventions,prompt").unwrap();
        for (id, t, p) in rows {
            writeln!(f, "{id},\"{}\",Female,9,Hispanic/Latino,Economically disadvantaged,Independent,train,3,3,3,3,3,3,3,{p}", t.replace('"', "\"\"")).unwrap();
        }
    }

    #[test]
    fn ellipse_run_screens_overlap_placeholders_and_shared_text() {
        let d = tempfile::tempdir().unwrap();
        let shared = "the quick brown fox jumps over the lazy dog every single morning without fail";
        let mut rows: Vec<(String, String, &str)> = Vec::new();
        // Five essays sharing one copied paragraph, each with own prose.
        for k in 0..5 {
            rows.push((format!("E{k}"), format!("{}\n\n{}\n\n{shared}. {shared}.", para(k, 120), para(k + 50, 120)), "Distance learning"));
        }
        rows.push(("P1".into(), format!("{}\n\n{}", para(9, 120), para(10, 120)), "Distance learning"));
        rows.push(("E9".into(), format!("Dear Generic_Name, hello there friend of mine today.\n\n{}\n\n{}", para(11, 100), para(12, 100)), "Distance learning"));
        let r: Vec<(&str, &str, &str)> = rows.iter().map(|(a, b, c)| (a.as_str(), b.as_str(), *c)).collect();
        let csv = d.path().join("train.csv");
        write_csv(&csv, &r);
        let ids = d.path().join("ids.jsonl");
        std::fs::write(&ids, "{\"essay_id_comp\":\"P1\",\"essay_id\":\"x\"}\n").unwrap();
        let out = d.path().join("out");
        let inputs = vec![Input { split: "train".into(), path: csv, password: None }];
        let mut run = Run::start_sampled(&out, "student-essays", ELLIPSE.collection, json!({}), None, None).unwrap();
        let o = Options { spec: &ELLIPSE, inputs: &inputs, out: &out, persuade_ids: Some(&ids), collection_terminus: None };
        let extra = adapt(&o, &mut run).unwrap();
        let rep = run.finish(extra).unwrap();
        assert_eq!(rep["rejection_reasons"]["same_essay_as_persuade_2_0_by_kaggle_id"], 1);
        assert_eq!(rep["extra"]["paragraph_breaks"]["shared_source_or_prompt_text"], 5);
        assert_eq!(rep["extra"]["paragraph_breaks"]["deidentification_placeholder"], 1);
        let rows: Vec<Value> = io::jsonl_rows(&out.join("candidates.jsonl")).unwrap().map(|r| r.unwrap()).collect();
        assert_eq!(rows.len(), 6);
        for r in &rows {
            let t = r["text"].as_str().unwrap();
            assert!(!t.contains("quick brown") && !t.contains("Generic_"));
            assert_eq!(r["provenance"]["writer_demographics"]["ell_status"], "Yes");
            assert_eq!(r["rights"]["redistribute_text"], false);
            assert_eq!(r["date"]["value"], "2022-08-31");
            assert!(r["source_group"].as_str().unwrap().starts_with("ellipse:essay:"));
        }
        let ids_out = std::fs::read_to_string(out.join("essay-ids.jsonl")).unwrap();
        assert!(!ids_out.contains("w1x"), "id list carries no text");
    }

    #[test]
    fn asap_rejects_undated_unless_terminus_given_and_reads_zip_tables() {
        let d = tempfile::tempdir().unwrap();
        let z = d.path().join("t.zip");
        {
            let mut w = zip::ZipWriter::new(std::fs::File::create(&z).unwrap());
            let opts = zip::write::SimpleFileOptions::default();
            w.start_file("__MACOSX/._t.csv", opts).unwrap();
            w.write_all(b"junk").unwrap();
            w.start_file("t.csv", opts).unwrap();
            let mut s = String::from("essay_id,score,full_text,set,pubpriv,assignment,prompt_name,economically_disadvantaged,student_disability_status,ell_status,race_ethnicity,gender,grade_level,essay_word_count,task\r\n");
            for k in 0..3 {
                s.push_str(&format!("A{k},3,\"{}\",train,0,x,Exploring Venus,NA,NA,No,White,M,10,200,Text dependent\r\n", para(k, 200)));
            }
            w.write_all(s.as_bytes()).unwrap();
            w.finish().unwrap();
        }
        let ids = d.path().join("ids.jsonl");
        std::fs::write(&ids, "{\"essay_id_comp\":\"Z\",\"essay_id\":\"A0\"}\n").unwrap();
        let inputs = vec![Input { split: "train".into(), path: z, password: None }];
        let out = d.path().join("o1");
        let mut run = Run::start_sampled(&out, "student-essays", ASAP2.collection, json!({}), None, None).unwrap();
        let o = Options { spec: &ASAP2, inputs: &inputs, out: &out, persuade_ids: Some(&ids), collection_terminus: None };
        let extra = adapt(&o, &mut run).unwrap();
        let rep = run.finish(extra).unwrap();
        assert_eq!(rep["candidates"], 0);
        assert_eq!(rep["rejection_reasons"]["same_essay_as_persuade_2_0_by_kaggle_id"], 1);
        assert_eq!(rep["rejection_reasons"]["no_composition_date_evidence_before_2022_11_30"], 2);
        let out2 = d.path().join("o2");
        let mut run = Run::start_sampled(&out2, "student-essays", ASAP2.collection, json!({}), None, None).unwrap();
        let o = Options { collection_terminus: Some(("2021-01-01", "test basis")), out: &out2, ..o };
        let extra = adapt(&o, &mut run).unwrap();
        let rep = run.finish(extra).unwrap();
        assert_eq!(rep["candidates"], 2);
    }

    #[test]
    fn buckets_gate_by_prompt() {
        let d = tempfile::tempdir().unwrap();
        let csv = d.path().join("train.csv");
        let rows: Vec<(String, String, &str)> = (0..4).map(|k| (format!("E{k}"), format!("{}\n\n{}", para(k, 120), para(k + 9, 120)), if k % 2 == 0 { "Driverless cars" } else { "Distance learning" })).collect();
        let r: Vec<(&str, &str, &str)> = rows.iter().map(|(a, b, c)| (a.as_str(), b.as_str(), *c)).collect();
        write_csv(&csv, &r);
        let ids = d.path().join("ids.jsonl");
        std::fs::write(&ids, "").unwrap();
        let inputs = vec![Input { split: "train".into(), path: csv, password: None }];
        let out = d.path().join("o");
        // driverless_cars hashes to bucket 968 and distance_learning to 270.
        assert_eq!(adapter::bucket(&prompt_key("Driverless cars")), 968);
        let mut run = Run::start_sampled(&out, "student-essays", ELLIPSE.collection, json!({}), None, Some((0, 900))).unwrap();
        let o = Options { spec: &ELLIPSE, inputs: &inputs, out: &out, persuade_ids: Some(&ids), collection_terminus: None };
        let extra = adapt(&o, &mut run).unwrap();
        let rep = run.finish(extra).unwrap();
        assert_eq!(rep["candidates"], 2);
        assert_eq!(rep["counted_without_ledger_rows"]["contributor_bucket_outside_run_range"], 2);
    }
}
