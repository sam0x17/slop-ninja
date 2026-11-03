//! RAID (Dugan et al., ACL 2024; https://github.com/liamdugan/raid) as a
//! removable Train cohort for A, plus the streaming CSV reader the RAID
//! leaderboard tooling shares.
//!
//! RAID rows are human references and title-conditioned generations from
//! eleven generators with four decoding settings and eleven adversarial
//! attacks. `adapt` maps rows of the labelled `train`/`extra` CSVs into the
//! pool document schema (`human_only` for human rows, `model_only` for
//! generated rows) with generator, decoding, repetition penalty and attack
//! metadata, and gates each component on rights:
//!
//! - the dataset itself is MIT licensed;
//! - human rows are admitted only from domains whose underlying source has
//!   no non-commercial restriction and dated composition before the
//!   cutoff (book summaries, Reddit, Wikipedia);
//! - generated rows are admitted for every English domain except those of a
//!   generator whose licence forbids commercial use (MPT-30B-Chat);
//! - non-English and code domains of `extra` are never admitted.
//!
//! `admit` removes every RAID source group with a text conflict against the
//! protected prior index (all earlier corpora and current pools), removes
//! rows that duplicate a row of another source group, and writes the pool.
//! A source group is a RAID `source_id`: one human document and every
//! generation and attack derived from its title stay together.
use crate::{
    io::{self, HashingReader, JsonlWriter},
    protected::Index,
    record, text,
};
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashMap},
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
};

pub const ADAPTER: &str = "raid";
pub const COLLECTION: &str = "raid_benchmark_v1";
/// Row licence id: the MIT dataset licence plus the per-row component terms
/// recorded under `rights.components`.
pub const LICENSE: &str = "MIT-dataset-compilation+component-terms-recorded-per-row";
pub const COHORT_FAMILY: &str = "raid-v1";
pub const COHORT_PAIRED: &str = "raid-v1-paired";
pub const COHORT_UNPAIRED: &str = "raid-v1-unpaired-domain";
pub const COHORT_PARAPHRASE: &str = "raid-v1-human-paraphrase";
pub const DATASET_URL: &str = "https://huggingface.co/datasets/liamdugan/raid";
pub const OFFICIAL_URL: &str = "https://dataset.raid-bench.xyz/";
pub const CITATION: &str = "Dugan et al. 2024, RAID: A Shared Benchmark for Robust Evaluation of Machine-Generated Text Detectors, ACL 2024";
pub const COLUMNS: [&str; 11] =
    ["id", "adv_source_id", "source_id", "model", "decoding", "repetition_penalty", "attack", "domain", "title", "prompt", "generation"];
pub const SAMPLE_DOMAIN: &str = "slop_ninja_raid_attack_sample_v1\0";
pub const RANK_DOMAIN: &str = "slop_ninja_raid_admit_rank_v1\0";
pub const ADMIT_METHOD: &str = "raid_group_prior_exclusion_then_cross_group_exact_normalized_folded_hash_and_folded_50_word_sequence_screen_plus_within_group_exact_duplicates_v1";

// ---------------------------------------------------------------- CSV

/// Streaming RFC 4180 reader over any `BufRead`: comma separators, quoted
/// fields with doubled quotes and embedded newlines, LF or CRLF record ends,
/// a leading UTF-8 byte-order mark skipped. Fields are returned exactly.
pub struct CsvStream<R: BufRead> {
    r: R,
    pub records: u64,
    first: bool,
}

enum St {
    Start,
    Unquoted,
    Quoted,
    QuoteSeen,
    Cr,
}

impl<R: BufRead> CsvStream<R> {
    pub fn new(r: R) -> Self {
        CsvStream { r, records: 0, first: true }
    }
    pub fn next_record(&mut self) -> Result<Option<Vec<String>>> {
        if self.first {
            self.first = false;
            let b = self.r.fill_buf()?;
            if b.starts_with(&[0xEF, 0xBB, 0xBF]) {
                self.r.consume(3);
            }
        }
        let mut fields: Vec<String> = Vec::new();
        let mut field: Vec<u8> = Vec::new();
        let mut st = St::Start;
        let mut any = false;
        let rec = self.records + 1;
        let push = |fields: &mut Vec<String>, field: &mut Vec<u8>| -> Result<()> {
            fields.push(String::from_utf8(std::mem::take(field)).with_context(|| format!("invalid UTF-8 in record {rec}"))?);
            Ok(())
        };
        loop {
            let (used, done) = {
                let buf = self.r.fill_buf()?;
                if buf.is_empty() {
                    if !any {
                        return Ok(None);
                    }
                    match st {
                        St::Quoted => bail!("unterminated quoted field in record {rec}"),
                        St::Cr => bail!("bare carriage return in record {rec}"),
                        _ => {}
                    }
                    push(&mut fields, &mut field)?;
                    self.records += 1;
                    return Ok(Some(fields));
                }
                let mut i = 0;
                let mut done = false;
                while i < buf.len() {
                    let c = buf[i];
                    i += 1;
                    any = true;
                    match st {
                        St::Quoted => {
                            // Fast path: copy up to the next quote.
                            let rest = &buf[i - 1..];
                            match rest.iter().position(|&x| x == b'"') {
                                Some(0) => st = St::QuoteSeen,
                                Some(p) => {
                                    field.extend_from_slice(&rest[..p]);
                                    i += p;
                                    st = St::QuoteSeen;
                                }
                                None => {
                                    field.extend_from_slice(rest);
                                    i = buf.len();
                                }
                            }
                        }
                        St::Start => match c {
                            b'"' => st = St::Quoted,
                            b',' => push(&mut fields, &mut field)?,
                            b'\n' => {
                                done = true;
                                break;
                            }
                            b'\r' => st = St::Cr,
                            _ => {
                                field.push(c);
                                st = St::Unquoted;
                            }
                        },
                        St::Unquoted => match c {
                            b',' => {
                                push(&mut fields, &mut field)?;
                                st = St::Start;
                            }
                            b'\n' => {
                                done = true;
                                break;
                            }
                            b'\r' => st = St::Cr,
                            b'"' => bail!("quote inside unquoted field in record {rec}"),
                            _ => field.push(c),
                        },
                        St::QuoteSeen => match c {
                            b'"' => {
                                field.push(b'"');
                                st = St::Quoted;
                            }
                            b',' => {
                                push(&mut fields, &mut field)?;
                                st = St::Start;
                            }
                            b'\n' => {
                                done = true;
                                break;
                            }
                            b'\r' => st = St::Cr,
                            _ => bail!("text after closing quote in record {rec}"),
                        },
                        St::Cr => match c {
                            b'\n' => {
                                done = true;
                                break;
                            }
                            _ => bail!("bare carriage return in record {rec}"),
                        },
                    }
                }
                (i, done)
            };
            self.r.consume(used);
            if done {
                push(&mut fields, &mut field)?;
                self.records += 1;
                return Ok(Some(fields));
            }
        }
    }
}

// ---------------------------------------------------------------- tables

/// One RAID generator and its stated terms.
pub struct Generator {
    pub raid: &'static str,
    pub label: &'static str,
    pub checkpoint: &'static str,
    pub family: &'static str,
    pub access: &'static str,
    pub terms: &'static str,
    pub terms_url: &'static str,
    /// None = admitted; Some(reason) = every row of this generator is excluded.
    pub excluded: Option<&'static str>,
    pub note: &'static str,
}

pub const GENERATORS: &[Generator] = &[
    Generator { raid: "gpt2", label: "gpt2-xl", checkpoint: "openai-community/gpt2-xl", family: "gpt2", access: "open_weights",
        terms: "MIT (modified MIT model licence)", terms_url: "https://huggingface.co/openai-community/gpt2-xl", excluded: None,
        note: "open weights under a permissive licence" },
    Generator { raid: "gpt3", label: "text-davinci-002", checkpoint: "openai:text-davinci-002", family: "openai_gpt", access: "hosted_api",
        terms: "OpenAI terms of use", terms_url: "https://openai.com/policies/terms-of-use", excluded: None,
        note: "generated by the RAID authors and released in the MIT dataset" },
    Generator { raid: "chatgpt", label: "gpt-3.5-turbo-0613", checkpoint: "openai:gpt-3.5-turbo-0613", family: "openai_gpt", access: "hosted_api",
        terms: "OpenAI terms of use", terms_url: "https://openai.com/policies/terms-of-use", excluded: None,
        note: "as text-davinci-002" },
    Generator { raid: "gpt4", label: "gpt-4-0613", checkpoint: "openai:gpt-4-0613", family: "openai_gpt", access: "hosted_api",
        terms: "OpenAI terms of use", terms_url: "https://openai.com/policies/terms-of-use", excluded: None,
        note: "as text-davinci-002" },
    Generator { raid: "llama-chat", label: "llama-2-70b-chat", checkpoint: "meta-llama/Llama-2-70b-chat-hf", family: "llama", access: "open_weights",
        terms: "Llama 2 Community License", terms_url: "https://ai.meta.com/llama/license/", excluded: None,
        note: "terms recorded as stated" },
    Generator { raid: "mistral", label: "mistral-7b-v0.1", checkpoint: "mistralai/Mistral-7B-v0.1", family: "mistral", access: "open_weights",
        terms: "Apache-2.0", terms_url: "https://huggingface.co/mistralai/Mistral-7B-v0.1", excluded: None, note: "" },
    Generator { raid: "mistral-chat", label: "mistral-7b-instruct-v0.1", checkpoint: "mistralai/Mistral-7B-Instruct-v0.1", family: "mistral", access: "open_weights",
        terms: "Apache-2.0", terms_url: "https://huggingface.co/mistralai/Mistral-7B-Instruct-v0.1", excluded: None, note: "" },
    Generator { raid: "mpt", label: "mpt-30b", checkpoint: "mosaicml/mpt-30b", family: "mpt", access: "open_weights",
        terms: "Apache-2.0", terms_url: "https://huggingface.co/mosaicml/mpt-30b", excluded: None, note: "" },
    Generator { raid: "mpt-chat", label: "mpt-30b-chat", checkpoint: "mosaicml/mpt-30b-chat", family: "mpt", access: "open_weights",
        terms: "CC-BY-NC-SA-4.0 (non-commercial)", terms_url: "https://huggingface.co/mosaicml/mpt-30b-chat",
        excluded: Some("generator_licence_non_commercial"), note: "model licence forbids commercial use; every row excluded" },
    Generator { raid: "cohere", label: "cohere-command-generate", checkpoint: "cohere:command (co.generate, unversioned, 2023-11)", family: "cohere", access: "hosted_api",
        terms: "Cohere terms of use", terms_url: "https://cohere.com/terms-of-use", excluded: None,
        note: "2023 API outputs (co.generate), not open-weight Command R outputs; terms recorded as stated" },
    Generator { raid: "cohere-chat", label: "cohere-command-chat", checkpoint: "cohere:command (co.chat, unversioned, 2023-11)", family: "cohere", access: "hosted_api",
        terms: "Cohere terms of use", terms_url: "https://cohere.com/terms-of-use", excluded: None,
        note: "as cohere-command-generate" },
];

pub fn generator(raid: &str) -> Option<&'static Generator> {
    GENERATORS.iter().find(|g| g.raid == raid)
}

/// One RAID domain and the rights and date status of its human source.
pub struct Domain {
    pub raid: &'static str,
    pub english: bool,
    pub human_source: &'static str,
    pub human_source_url: &'static str,
    pub human_terms: &'static str,
    /// Licence id for admitted human rows.
    pub human_license: Option<&'static str>,
    pub human_date: Option<&'static str>,
    pub human_date_basis: &'static str,
    /// None = human rows admitted; Some(reason) = human rows excluded.
    pub human_excluded: Option<&'static str>,
}

pub const DOMAINS: &[Domain] = &[
    Domain { raid: "abstracts", english: true, human_source: "arXiv abstracts (Kaggle arXiv paper abstracts, Paul and Rakshit 2021), filtered by RAID to papers from 2023 or later",
        human_source_url: "https://www.kaggle.com/datasets/spsayakpaul/arxiv-paper-abstracts", human_terms: "arXiv metadata CC0-1.0",
        human_license: None, human_date: None, human_date_basis: "RAID states a 2023-or-later filter for this domain",
        human_excluded: Some("human_date_not_established_before_cutoff") },
    Domain { raid: "books", english: true, human_source: "CMU Book Summary Dataset (Bamman and Smith 2013): Wikipedia plot summaries",
        human_source_url: "https://www.cs.cmu.edu/~dbamman/booksummaries.html", human_terms: "CC BY-SA 3.0 (dataset page: all data released under a Creative Commons Attribution-ShareAlike License)",
        human_license: Some("CC-BY-SA-3.0"), human_date: Some("2013-12-31"), human_date_basis: "upper_bound_dataset_release_2013", human_excluded: None },
    Domain { raid: "news", english: true, human_source: "BBC News dataset (Greene and Cunningham 2006), articles 2004-2005",
        human_source_url: "http://mlg.ucd.ie/datasets/bbc.html", human_terms: "made available for non-commercial and research purposes only; article copyright owned by the BBC",
        human_license: None, human_date: None, human_date_basis: "", human_excluded: Some("human_source_terms_non_commercial") },
    Domain { raid: "poetry", english: true, human_source: "Poems Dataset (NLP) on Kaggle (Arman 2020), collected from poemhunter.com",
        human_source_url: "https://www.kaggle.com/datasets/michaelarman/poemsdataset", human_terms: "poems owned by their authors; the site states they are reproduced for educational and informational purposes; the uploader labels the dataset CC0",
        human_license: None, human_date: None, human_date_basis: "", human_excluded: Some("human_source_terms_educational_informational_purpose_only") },
    Domain { raid: "recipes", english: true, human_source: "RecipeNLG (Bien et al. 2020)",
        human_source_url: "https://recipenlg.cs.put.poznan.pl/", human_terms: "use the dataset only for non-commercial research and educational purposes",
        human_license: None, human_date: None, human_date_basis: "", human_excluded: Some("human_source_terms_non_commercial") },
    Domain { raid: "reddit", english: true, human_source: "Webis-TLDR-17 (Voelske et al. 2017) Reddit posts 2006-2016",
        human_source_url: "https://zenodo.org/records/1043504", human_terms: "compilation CC BY 4.0; post text is Reddit user content; basis: classifier-training use, user decision 2026-10-27",
        human_license: Some(crate::forum::WEBIS_LICENSE), human_date: Some("2017-11-07"), human_date_basis: "upper_bound_corpus_publication_2017-11-07;posts_2006_2016", human_excluded: None },
    Domain { raid: "reviews", english: true, human_source: "IMDb Large Movie Review Dataset (Maas et al. 2011)",
        human_source_url: "https://ai.stanford.edu/~amaas/data/sentiment/", human_terms: "no dataset licence; IMDb conditions of use",
        human_license: None, human_date: None, human_date_basis: "", human_excluded: Some("human_source_terms_non_commercial") },
    Domain { raid: "wiki", english: true, human_source: "GPT-wiki-intro (Bhat 2023) wiki_intro column, from the Hugging Face wikipedia 20220301.en dump",
        human_source_url: "https://huggingface.co/datasets/aadityaubhat/GPT-wiki-intro", human_terms: "Wikipedia text CC BY-SA 3.0",
        human_license: Some("CC-BY-SA-3.0"), human_date: Some("2022-03-01"), human_date_basis: "wikipedia_dump_20220301.en_per_source_notebook", human_excluded: None },
    Domain { raid: "code", english: false, human_source: "Python solutions (Raychev et al. 2016)", human_source_url: "", human_terms: "", human_license: None,
        human_date: None, human_date_basis: "", human_excluded: Some("code_not_prose") },
    Domain { raid: "czech", english: false, human_source: "Czech news (Bohacek et al. 2022)", human_source_url: "", human_terms: "", human_license: None,
        human_date: None, human_date_basis: "", human_excluded: Some("not_english") },
    Domain { raid: "german", english: false, human_source: "German news, One Million Posts / DER STANDARD (Schabus et al. 2017)", human_source_url: "", human_terms: "", human_license: None,
        human_date: None, human_date_basis: "", human_excluded: Some("not_english") },
];

pub fn domain(raid: &str) -> Option<&'static Domain> {
    DOMAINS.iter().find(|d| d.raid == raid)
}

/// RAID attack names, the mutation rate RAID reports (Table 11) and what
/// produced the final text.
pub const ATTACKS: &[(&str, &str, &str)] = &[
    ("none", "", "none"),
    ("alternative_spelling", "1.00", "rule: British/American spelling swap"),
    ("article_deletion", "0.50", "rule: article deletion"),
    ("homoglyph", "1.00", "rule: homoglyph character substitution"),
    ("insert_paragraphs", "0.50", "rule: paragraph break insertion"),
    ("number", "0.50", "rule: digit swap"),
    ("paraphrase", "1.00", "model: DIPPER paraphraser (kalpeshk2011/dipper-paraphraser-xxl, Apache-2.0)"),
    ("perplexity_misspelling", "0.20", "rule misspellings ranked by GPT-2 small likelihood"),
    ("synonym", "0.50", "model: BERT mask-fill synonym substitution"),
    ("upper_lower", "0.05", "rule: case swap"),
    ("whitespace", "0.20", "rule: whitespace insertion"),
    ("zero_width_space", "1.00", "rule: zero-width space insertion"),
];

pub fn attack(name: &str) -> Option<(&'static str, &'static str, &'static str)> {
    ATTACKS.iter().copied().find(|a| a.0 == name)
}

// ---------------------------------------------------------------- shape

/// Whole text inside the bound, or the first run of whole paragraphs
/// (blank-line separated) that reaches the target. Outer whitespace is
/// trimmed; text inside the kept paragraphs is never changed.
pub fn fit(raw: &str) -> std::result::Result<(String, Option<[usize; 2]>, bool), &'static str> {
    let t = raw.trim();
    let n = text::words(t);
    if n < text::MIN_WORDS {
        return Err("word_count_below_150");
    }
    if n <= text::MAX_WORDS {
        return Ok((t.to_owned(), None, true));
    }
    let paras: Vec<text::Para> = paragraphs(t).into_iter().map(|p| text::Para { text: p.to_owned(), prose: true, reason: None }).collect();
    match text::paragraph_run(&paras) {
        Some(ex) => Ok((ex.text, Some([ex.start, ex.end]), false)),
        None => Err("no_whole_paragraph_run_within_150_450"),
    }
}

/// Paragraphs separated by one or more whitespace-only lines.
pub fn paragraphs(t: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start: Option<usize> = None;
    let mut end = 0usize;
    let mut pos = 0usize;
    for line in t.split_inclusive('\n') {
        let blank = line.trim().is_empty();
        if blank {
            if let Some(s) = start.take() {
                out.push(&t[s..end]);
            }
        } else {
            if start.is_none() {
                start = Some(pos);
            }
            end = pos + line.trim_end_matches(['\n', '\r']).len();
        }
        pos += line.len();
    }
    if let Some(s) = start {
        out.push(&t[s..end]);
    }
    out
}

/// Minimum English function-word share for RAID rows (see `adapt`).
pub const MIN_STOPWORD_SHARE: f64 = 0.10;

/// Language screen for RAID rows: the shared Latin-letter floor and a lower
/// function-word floor than the book screen.
pub fn english_screen(s: &str) -> Option<&'static str> {
    if text::latin_letter_share(s) < text::MIN_LATIN_LETTER_SHARE {
        return Some("latin_letter_share_below_0_90");
    }
    if text::english_stopword_share(s) < MIN_STOPWORD_SHARE {
        return Some("english_stopword_share_below_0_10");
    }
    None
}

/// Share of word 8-grams that repeat an earlier 8-gram (degenerate loops).
pub fn repeated_8gram_share(t: &str) -> f64 {
    let w: Vec<&str> = t.split_whitespace().collect();
    if w.len() < 8 {
        return 0.0;
    }
    let mut seen = std::collections::HashSet::new();
    let mut rep = 0usize;
    let total = w.len() - 7;
    for g in w.windows(8) {
        if !seen.insert(g) {
            rep += 1;
        }
    }
    rep as f64 / total as f64
}

/// Lines of a multi-line paragraph that look hard-wrapped (arXiv style).
pub fn hard_wrapped(t: &str) -> bool {
    paragraphs(t).iter().any(|p| {
        let lines: Vec<&str> = p.lines().collect();
        lines.len() >= 4 && lines[..lines.len() - 1].iter().all(|l| (50..=82).contains(&l.chars().count()))
    })
}

/// Line-level formatting flags recorded for weighting decisions.
pub fn format_flags(t: &str) -> Value {
    let lines: Vec<&str> = t.lines().map(str::trim_start).filter(|l| !l.is_empty()).collect();
    let list = lines.iter().filter(|l| {
        l.starts_with("- ") || l.starts_with("* ") || l.starts_with("• ") || {
            let d = l.chars().take_while(char::is_ascii_digit).count();
            d > 0 && d <= 3 && l[d..].starts_with(['.', ')'])
        }
    }).count();
    let heading = lines.iter().filter(|l| l.starts_with('#') || (l.ends_with(':') && text::words(l) <= 6)).count();
    json!({"lines": lines.len(), "list_lines": list, "heading_like_lines": heading, "hard_wrapped": hard_wrapped(t),
           "repeated_word_8gram_share": (repeated_8gram_share(t) * 1e6).round() / 1e6})
}

// ---------------------------------------------------------------- adapt

pub struct AdaptOptions<'a> {
    pub inputs: &'a [(PathBuf, String, String)],
    pub out: &'a Path,
    pub attack_per_mille: u32,
    pub max_rows: Option<u64>,
}

fn sample_attack(id: &str, per_mille: u32) -> bool {
    (text::h64(format!("{SAMPLE_DOMAIN}{id}").as_bytes()) % 1000) < per_mille as u64
}

#[derive(Default)]
struct Tally(BTreeMap<String, BTreeMap<String, u64>>);
impl Tally {
    fn add(&mut self, axis: &str, key: &str) {
        *self.0.entry(axis.to_owned()).or_default().entry(key.to_owned()).or_default() += 1;
    }
}

/// Map labelled RAID CSVs (`train`, `extra`) to candidate rows.
/// `inputs`: (path, RAID split name, expected SHA-256 of the decompressed CSV).
pub fn adapt(o: &AdaptOptions) -> Result<Value> {
    io::fresh_dir(o.out)?;
    let probe_dir = o.out.join("probe");
    std::fs::create_dir_all(&probe_dir)?;
    let mut cands = JsonlWriter::create(&o.out.join("candidates.jsonl"))?;
    let mut probe = JsonlWriter::create(&probe_dir.join("candidates.jsonl"))?;
    let mut ledger = JsonlWriter::create(&o.out.join("rejections.jsonl"))?;
    let mut seen = Tally::default();
    let mut kept = Tally::default();
    let mut reasons: BTreeMap<String, u64> = BTreeMap::new();
    let mut inputs = Vec::new();
    let mut words = Vec::new();
    let mut total = 0u64;
    'files: for (path, split, expect) in o.inputs {
        let f = std::fs::File::open(path).with_context(|| format!("open {}", path.display()))?;
        let dec: Box<dyn std::io::Read> = if path.extension().is_some_and(|e| e == "zst") { Box::new(zstd::Decoder::new(f)?) } else { Box::new(f) };
        let mut hr = HashingReader::new(dec);
        let mut rows_in_file = 0u64;
        {
            let mut csv = CsvStream::new(BufReader::with_capacity(1 << 22, &mut hr));
            let header = csv.next_record()?.context("empty CSV")?;
            ensure!(header == COLUMNS, "unexpected RAID header in {}: {:?}", path.display(), header);
            while let Some(r) = csv.next_record()? {
                ensure!(r.len() == COLUMNS.len(), "record {} has {} fields", csv.records, r.len());
                rows_in_file += 1;
                total += 1;
                let [id, adv, src, model, decoding, rp, att, dom, title, _prompt, gen_text] = <[String; 11]>::try_from(r).expect("width checked");
                let is_human = model == "human";
                let stratum = format!("{split}|{dom}|{model}|{decoding}|{rp}|{att}");
                seen.add("stratum", &stratum);
                let mut reject = |reason: &str, ledger_row: bool| -> Result<()> {
                    *reasons.entry(reason.to_owned()).or_default() += 1;
                    seen.add("rejected_by_reason_domain", &format!("{reason}|{dom}"));
                    if ledger_row {
                        ledger.write(&json!({"raid_id": id, "raid_split": split, "reason": reason}))?;
                    }
                    Ok(())
                };
                let d = domain(&dom).with_context(|| format!("unknown RAID domain {dom}"))?;
                let (_, theta, editor) = attack(&att).with_context(|| format!("unknown RAID attack {att}"))?;
                let g = if is_human { None } else { Some(generator(&model).with_context(|| format!("unknown RAID model {model}"))?) };
                if !d.english {
                    reject(d.human_excluded.unwrap_or("not_english"), false)?;
                    continue;
                }
                if let Some(g) = g
                    && let Some(why) = g.excluded
                {
                    reject(why, false)?;
                    continue;
                }
                // Human-derived rows need an admitted human source.
                let paraphrased_human = is_human && att == "paraphrase";
                if is_human && let Some(why) = d.human_excluded {
                    reject(why, false)?;
                    continue;
                }
                if is_human && att == "synonym" {
                    reject("human_text_with_model_substituted_words_spans_unrecorded", false)?;
                    continue;
                }
                let attacked = att != "none";
                if attacked && !sample_attack(&id, o.attack_per_mille) {
                    reject("attacked_row_outside_seeded_sample", false)?;
                    continue;
                }
                let (txt, span, complete) = match fit(&gen_text) {
                    Ok(x) => x,
                    Err(r) => {
                        reject(r, true)?;
                        continue;
                    }
                };
                if let Some(r) = text::damaged(&txt) {
                    reject(r, true)?;
                    continue;
                }
                if att != "homoglyph" && let Some(r) = english_screen(&txt) {
                    reject(r, true)?;
                    continue;
                }
                let row = build_row(&RowIn {
                    id: &id, adv: &adv, src: &src, split, d, g, decoding: &decoding, rp: &rp, att: &att, theta, editor,
                    title: &title, raw: &gen_text, text: &txt, span, complete, paraphrased_human,
                });
                let n = text::words(&txt);
                words.push(n);
                kept.add("origin", row["origin"].as_str().unwrap_or(""));
                kept.add("domain", &dom);
                kept.add("generator", if is_human { "human" } else { g.map(|g| g.label).unwrap_or("") });
                kept.add("attack", &att);
                kept.add("decoding", &format!("{decoding}|rp={rp}"));
                kept.add("cohort", row["cohort_id"].as_str().unwrap_or(""));
                cands.write(&row)?;
                if !attacked {
                    probe.write(&json!({"id": row["id"], "text": row["text"]}))?;
                }
                if o.max_rows.is_some_and(|m| cands.rows >= m) {
                    break 'files;
                }
            }
        }
        let (sha, bytes) = hr.finish();
        let full = o.max_rows.is_none();
        if full {
            ensure!(sha == *expect, "decompressed SHA-256 of {} is {sha}, expected {expect}", path.display());
        }
        inputs.push(json!({"path": path, "raid_split": split, "decompressed_sha256": sha, "decompressed_bytes": bytes,
                           "expected_sha256": expect, "verified": full && sha == *expect, "rows": rows_in_file}));
    }
    let (cand_sha, cand_rows) = cands.finish()?;
    let (probe_sha, probe_rows) = probe.finish()?;
    let (rej_sha, rej_rows) = ledger.finish()?;
    words.sort_unstable();
    let stats = if words.is_empty() { json!(null) } else {
        json!({"min": words[0], "median": words[words.len() / 2], "max": words[words.len() - 1], "mean": words.iter().sum::<usize>() as f64 / words.len() as f64})
    };
    let report = json!({
        "schema": crate::adapter::REPORT_SCHEMA,
        "adapter": ADAPTER,
        "collection": COLLECTION,
        "finished_utc": io::now_utc(),
        "complete": o.max_rows.is_none(),
        "parameters": {"attack_per_mille": o.attack_per_mille, "attack_sample_rule": format!("keep an attacked row iff (first 8 bytes of SHA-256(\"{}\\0\" + RAID id) as big-endian u64) mod 1000 < attack_per_mille", SAMPLE_DOMAIN.trim_end_matches('\0')),
                       "max_rows": o.max_rows, "word_bound": [text::MIN_WORDS, text::MAX_WORDS], "shape_rule": "outer whitespace trimmed; whole text if 150..=450 whitespace words, else the first run of whole blank-line-separated paragraphs reaching 220 words (150..=450)",
                       "english_screen": "latin letter share >= 0.90 and English function-word share >= 0.10 (homoglyph-attacked rows exempt); RAID domains are English by construction, so the lower function-word floor removes only degenerate output and keeps dense abstracts"},
        "inputs": inputs,
        "units_seen": total,
        "candidates": cand_rows,
        "candidates_sha256": cand_sha,
        "probe": {"file": "probe/candidates.jsonl", "rows": probe_rows, "sha256": probe_sha, "rule": "unattacked candidates only; prior conflicts exclude whole source groups, so attacked variants follow their group"},
        "rejections": rej_rows,
        "rejections_sha256": rej_sha,
        "rejection_reasons": reasons,
        "seen": seen.0,
        "kept": kept.0,
        "candidate_word_counts": stats,
        "detector_calls": 0,
        "selection_by_model_or_detector_score": false,
        "split_assigned": false,
    });
    io::write_json(&o.out.join("report.json"), &report)?;
    Ok(json!({"candidates": cand_rows, "probe": probe_rows, "units_seen": total, "rejection_reasons": report["rejection_reasons"], "kept": report["kept"]}))
}

struct RowIn<'a> {
    id: &'a str,
    adv: &'a str,
    src: &'a str,
    split: &'a str,
    d: &'static Domain,
    g: Option<&'static Generator>,
    decoding: &'a str,
    rp: &'a str,
    att: &'a str,
    theta: &'a str,
    editor: &'a str,
    title: &'a str,
    raw: &'a str,
    text: &'a str,
    span: Option<[usize; 2]>,
    complete: bool,
    paraphrased_human: bool,
}

fn build_row(r: &RowIn) -> Value {
    let t = r.text;
    let human = r.g.is_none() && !r.paraphrased_human;
    let (origin, share, spans) = if human { ("human_only", 0.0, json!([])) } else { ("model_only", 1.0, json!([[0, t.len()]])) };
    let cohort = if r.paraphrased_human {
        COHORT_PARAPHRASE
    } else if r.d.human_excluded.is_none() {
        COHORT_PAIRED
    } else {
        COHORT_UNPAIRED
    };
    let (label, key, operation) = match (r.g, r.paraphrased_human) {
        (Some(g), _) => (Some(g.label), Some(format!("raid:{}", g.raid)), "title_conditioned_generation"),
        (None, true) => (Some("dipper-paraphraser-xxl"), Some("raid:attack:paraphrase".to_owned()), "source_conditioned_paraphrase_of_human_text"),
        (None, false) => (None, None, "human_reference"),
    };
    let human_component = json!({"source": r.d.human_source, "url": r.d.human_source_url, "terms": r.d.human_terms,
        "license": r.d.human_license, "admitted": r.d.human_excluded.is_none(), "excluded_reason": r.d.human_excluded});
    let generator_component = r.g.map(|g| json!({"label": g.label, "checkpoint": g.checkpoint, "family": g.family, "access": g.access,
        "terms": g.terms, "terms_url": g.terms_url, "note": g.note}));
    let attack_component = if r.att == "paraphrase" {
        json!({"model": "kalpeshk2011/dipper-paraphraser-xxl", "license": "Apache-2.0"})
    } else {
        json!(null)
    };
    let license = if human { r.d.human_license.unwrap_or(LICENSE) } else { LICENSE };
    let basis = if r.d.raid == "reddit" && human {
        crate::forum::ADMISSION_BASIS.to_owned()
    } else {
        "RAID dataset MIT licence; every component checked for non-commercial terms (user decisions 2026-10-27 and 2026-10-28)".to_owned()
    };
    let decoding = if human && !r.paraphrased_human {
        json!(null)
    } else if r.paraphrased_human {
        json!({"strategy": "dipper_default", "raid_decoding": null, "repetition_penalty": null})
    } else {
        json!({"strategy": r.decoding, "temperature": if r.decoding == "greedy" { json!(0.0) } else { json!(1.0) },
               "repetition_penalty": r.rp == "yes", "repetition_penalty_theta": if r.rp == "yes" { json!(1.2) } else { json!(null) },
               "raid_decoding": r.decoding, "raid_repetition_penalty": r.rp})
    };
    let date = if human { json!({"value": r.d.human_date, "basis": r.d.human_date_basis}) } else {
        json!({"value": "2023-11-15", "basis": "upper_bound_raid_generation_run_2023-11-01_to_2023-11-15"})
    };
    let cutoff = if human { json!({"policy": "dated_before_cutoff", "before": record::CUTOFF}) } else {
        json!({"policy": "model_output_not_cutoff_gated", "before": record::CUTOFF})
    };
    let raid_meta = json!({
            "split": r.split, "id": r.id, "source_id": r.src, "adv_source_id": r.adv, "domain": r.d.raid,
            "model": if human { "human" } else { r.g.map(|g| g.raid).unwrap_or("") },
            "decoding": decoding,
            "attack": {"name": r.att, "attacked": r.att != "none", "raid_mutation_rate": if r.theta.is_empty() { json!(null) } else { json!(r.theta) },
                       "final_text_editor": r.editor},
            "title": r.title,
            "raw_text_sha256": text::sha_hex(r.raw.as_bytes()),
        });
    let extraction = json!({
            "method": "raid_generation_field;outer_whitespace_trimmed;whole_if_150_450_else_first_blank_line_paragraph_run_reaching_220_words_v1",
            "unit": {"kind": "raid_row", "raid_id": r.id, "raid_split": r.split, "span_basis": "blank_line_paragraph_index_half_open"},
            "span": r.span,
            "span_basis": "blank_line_paragraph_index_half_open",
            "complete_unit": r.complete,
            "no_truncation_inside_paragraph_or_sentence": true,
            "no_prose_repair": true,
        });
    let rights = json!({
            "license": license,
            "dataset_license": "MIT (RAID repository and Hugging Face dataset card)",
            "components": {"human_source": human_component, "generator_output": generator_component, "attack_model": attack_component},
            "admission_basis": basis,
            "admission_decision": "admitted: no component carries a non-commercial restriction",
            "attribution": format!("RAID benchmark row {} ({})", r.id, CITATION),
            "evidence_url": DATASET_URL,
            "cohort_id": cohort,
            "commercial_training": true,
            "model_release": true,
            "external_evaluation": false,
            "evaluation_use": "train_only",
            "redistribute_text": false,
            "removable_cohort": true,
            "notes": "Public benchmark text: Train only, never an internal evaluation split; the RAID test split is evaluated only through its leaderboard.",
        });
    let mut head = json!({
        "schema": record::DOCUMENT_SCHEMA,
        "id": format!("raid-{}", r.id),
        "collection": COLLECTION,
        "domain": format!("raid_{}", r.d.raid),
        "genre": r.d.raid,
        "source_group": format!("raid_source:{}", r.src),
        "source_group_policy": "One RAID source document: its human text and every generation and attack derived from its title share this group and its split.",
        "writer_key": null,
        "aliases": [],
        "origin": origin,
        "evidence": if human { "public_benchmark_human_label" } else { "public_benchmark_generation_label" },
        "documented_human_only": false,
        "human_role_present": human,
        "ai_role_present": !human,
        "ai_identity": label,
        "generator_label": label,
        "generator_key": key,
        "operation": operation,
        "share_target": share,
        "share_evidence": if human { json!({"basis": "historical_human_proxy_endpoint", "not_documented_contemporary_workflow": true}) }
            else { json!({"basis": "whole_text_model_produced", "exact_spans": true}) },
        "ai_byte_spans": spans,
        "cohort_id": cohort,
        "intended_use": "train_only",
        "raid": raid_meta,
    });
    let tail = json!({
        "format": format_flags(t),
        "text": t,
        "text_sha256": text::sha_hex(t.as_bytes()),
        "normalized_text_sha256": text::normalized_sha(t),
        "folded_text_sha256": text::folded_sha(t),
        "word_count": text::words(t),
        "word_count_basis": "whitespace_tokens",
        "word_bound": [text::MIN_WORDS, text::MAX_WORDS],
        "word_bound_is_not_tokenizer_qualification": true,
        "extraction": extraction,
        "provenance": {"benchmark": "RAID", "citation": CITATION, "dataset_url": DATASET_URL, "official_url": OFFICIAL_URL,
                       "raid_split": r.split, "raid_id": r.id, "cohort_family": COHORT_FAMILY, "cohort_id": cohort},
        "date": date,
        "cutoff": cutoff,
        "rights": rights,
        "exposure": {"pretraining_exposure": "unknown; public benchmark since 2024",
                     "grammar_exposure": "none_recorded;fresh_pool_is_not_an_exposure_audit",
                     "human_only_production_history": if human { "not_authenticated" } else { "not_applicable" }},
        "pool_status": "candidate",
        "frozen_split": null,
        "split_assigned": false,
    });
    if let (Some(h), Value::Object(t)) = (head.as_object_mut(), tail) {
        h.extend(t);
    }
    head
}

// ---------------------------------------------------------------- admit

pub struct AdmitOptions<'a> {
    pub candidates: &'a Path,
    pub index: &'a Path,
    pub out: &'a Path,
}

struct Cand {
    group: u32,
    rank: u64,
    keys: [u64; 3],
    prior: bool,
}

struct Uf(Vec<u32>);
impl Uf {
    fn find(&mut self, x: u32) -> u32 {
        let mut r = x;
        while self.0[r as usize] != r {
            r = self.0[r as usize];
        }
        let mut c = x;
        while self.0[c as usize] != r {
            let n = self.0[c as usize];
            self.0[c as usize] = r;
            c = n;
        }
        r
    }
    fn union(&mut self, a: u32, b: u32) {
        let (ra, rb) = (self.find(a), self.find(b));
        if ra != rb {
            self.0[ra.max(rb) as usize] = ra.min(rb);
        }
    }
}

/// Admit candidates into the pool directory `out`.
pub fn admit(o: &AdmitOptions) -> Result<Value> {
    io::fresh_dir(o.out)?;
    let index = Index::load(o.index)?;
    let cand_dir = o.candidates;
    let rep: Value = serde_json::from_slice(&std::fs::read(cand_dir.join("report.json"))?)?;
    ensure!(rep["schema"] == crate::adapter::REPORT_SCHEMA && rep["complete"] == true && rep["adapter"] == ADAPTER, "incomplete RAID adapter report");
    let cand_path = cand_dir.join("candidates.jsonl");
    let (sha, _) = io::file_sha(&cand_path)?;
    ensure!(rep["candidates_sha256"] == sha.as_str(), "candidates changed since the adapter report");

    // Pass 1: keys, prior conflicts and folded 50-word sequences.
    let mut groups: HashMap<String, u32> = HashMap::new();
    let mut group_names: Vec<String> = Vec::new();
    let mut cands: Vec<Cand> = Vec::new();
    let mut shingles: Vec<(u64, u32)> = Vec::new();
    let mut prior_reasons: BTreeMap<String, u64> = BTreeMap::new();
    for row in io::jsonl_rows(&cand_path)? {
        let row = row?;
        let t = row["text"].as_str().context("text")?;
        ensure!(row["text_sha256"] == text::sha_hex(t.as_bytes()), "text hash mismatch");
        let g = row["source_group"].as_str().context("source_group")?;
        let next = group_names.len() as u32;
        let gid = *groups.entry(g.to_owned()).or_insert_with(|| {
            group_names.push(g.to_owned());
            next
        });
        let mut prior = index.text_conflicts(t);
        prior.extend(index.family_conflicts(&row));
        for r in &prior {
            *prior_reasons.entry((*r).to_owned()).or_default() += 1;
        }
        let i = cands.len() as u32;
        for s in text::shingles_folded(t) {
            shingles.push((s, i));
        }
        cands.push(Cand {
            group: gid,
            rank: text::h64(format!("{RANK_DOMAIN}{}", row["id"].as_str().unwrap_or("")).as_bytes()),
            keys: text::text_keys(t),
            prior: !prior.is_empty(),
        });
    }
    let n = cands.len();
    let mut prior_group = vec![false; group_names.len()];
    for c in &cands {
        if c.prior {
            prior_group[c.group as usize] = true;
        }
    }
    // Earliest-ranked owner per key; a row is a cross-group duplicate when an
    // earlier-ranked row of another (non-prior) group shares a key with it.
    let order_key = |i: u32| (cands[i as usize].rank, i);
    let mut dup_of: Vec<Option<(u32, &'static str)>> = vec![None; n];
    let mut uf = Uf((0..group_names.len() as u32).collect());
    let mut cross_links = 0u64;
    let mark = |dup_of: &mut Vec<Option<(u32, &'static str)>>, uf: &mut Uf, members: &mut Vec<u32>, reason: &'static str, within_group_exact: bool| {
        members.retain(|&i| !prior_group[cands[i as usize].group as usize]);
        members.sort_by_key(|&i| order_key(i));
        members.dedup();
        if members.len() < 2 {
            return;
        }
        let first = members[0];
        let g0 = cands[first as usize].group;
        let second = members.iter().copied().find(|&j| cands[j as usize].group != g0);
        for &i in &members[1..] {
            let gi = cands[i as usize].group;
            // Earliest-ranked member of another group, if it precedes i.
            let other = if gi != g0 { Some(first) } else { second.filter(|&j| order_key(j) < order_key(i)) };
            if let Some(j) = other {
                uf.union(gi, cands[j as usize].group);
                if dup_of[i as usize].is_none() {
                    dup_of[i as usize] = Some((j, reason));
                }
            } else if within_group_exact && dup_of[i as usize].is_none() {
                dup_of[i as usize] = Some((first, "within_group_exact_duplicate"));
            }
        }
    };
    for k in 0..3 {
        let mut v: Vec<(u64, u32)> = cands.iter().enumerate().map(|(i, c)| (c.keys[k], i as u32)).collect();
        v.sort_unstable();
        let labels = ["cross_group_exact_duplicate", "cross_group_whitespace_normalized_duplicate", "cross_group_folded_duplicate"];
        for run in v.chunk_by(|a, b| a.0 == b.0) {
            if run.len() > 1 {
                let mut m: Vec<u32> = run.iter().map(|x| x.1).collect();
                mark(&mut dup_of, &mut uf, &mut m, labels[k], k == 0);
            }
        }
    }
    shingles.sort_unstable();
    for run in shingles.chunk_by(|a, b| a.0 == b.0) {
        if run.len() > 1 {
            let gs = run.iter().map(|x| cands[x.1 as usize].group).collect::<std::collections::BTreeSet<_>>();
            if gs.len() > 1 {
                let mut m: Vec<u32> = run.iter().map(|x| x.1).collect();
                mark(&mut dup_of, &mut uf, &mut m, "cross_group_shares_folded_50_word_sequence", false);
                cross_links += 1;
            }
        }
    }
    drop(shingles);
    let mut comp_key: HashMap<u32, String> = HashMap::new();
    let mut comp_size: HashMap<u32, u32> = HashMap::new();
    for g in 0..group_names.len() as u32 {
        let r = uf.find(g);
        let e = comp_key.entry(r).or_insert_with(|| group_names[g as usize].clone());
        if group_names[g as usize] < *e {
            *e = group_names[g as usize].clone();
        }
        *comp_size.entry(r).or_default() += 1;
    }

    // Pass 2: write the pool.
    let mut docs = JsonlWriter::create(&o.out.join("documents.jsonl"))?;
    let mut meta = JsonlWriter::create(&o.out.join("metadata.jsonl"))?;
    std::fs::create_dir_all(o.out.join("admission"))?;
    let mut excl = JsonlWriter::create(&o.out.join("admission").join("exclusions.jsonl"))?;
    let mut counts = Tally::default();
    let mut excluded_reasons: BTreeMap<String, u64> = BTreeMap::new();
    let mut admitted_groups = std::collections::BTreeSet::new();
    let mut words = Vec::new();
    for (i, row) in io::jsonl_rows(&cand_path)?.enumerate() {
        let mut row = row?;
        let c = &cands[i];
        let root = uf.find(c.group);
        let reason: Option<(String, Option<u32>)> = if prior_group[c.group as usize] {
            Some((if c.prior { "prior_snapshot_text_or_family_conflict" } else { "source_group_has_prior_snapshot_conflict" }.to_owned(), None))
        } else {
            dup_of[i].map(|(j, r)| (r.to_owned(), Some(j)))
        };
        let genl = row["generator_label"].as_str().unwrap_or("human").to_owned();
        let att = row["raid"]["attack"]["name"].as_str().unwrap_or("").to_owned();
        let dom = row["raid"]["domain"].as_str().unwrap_or("").to_owned();
        if let Some((r, dup)) = reason {
            *excluded_reasons.entry(r.clone()).or_default() += 1;
            counts.add("excluded_by_reason_domain", &format!("{r}|{dom}"));
            excl.write(&json!({"id": row["id"], "source_group": row["source_group"], "text_sha256": row["text_sha256"], "reason": r,
                               "duplicate_of_candidate_index": dup}))?;
            continue;
        }
        row["pool_status"] = json!("admitted");
        row["pool_admission"] = json!({"method": ADMIT_METHOD, "protected_index_sha256": index.sha256,
            "split_component": comp_key[&root], "component_groups": comp_size[&root]});
        admitted_groups.insert(c.group);
        words.push(row["word_count"].as_u64().unwrap_or(0) as usize);
        counts.add("origin", row["origin"].as_str().unwrap_or(""));
        counts.add("generator", &genl);
        counts.add("attack", &att);
        counts.add("domain", &dom);
        counts.add("cohort", row["cohort_id"].as_str().unwrap_or(""));
        counts.add("decoding", &match &row["raid"]["decoding"] {
            Value::Null => "none".to_owned(),
            d => format!("{}|rp={}", d["strategy"].as_str().unwrap_or(""), d["raid_repetition_penalty"].as_str().unwrap_or("-")),
        });
        counts.add("generator_by_domain", &format!("{genl}|{dom}"));
        counts.add("generator_by_attack", &format!("{genl}|{att}"));
        counts.add("domain_by_origin", &format!("{dom}|{}", row["origin"].as_str().unwrap_or("")));
        if row["format"]["repeated_word_8gram_share"].as_f64().is_some_and(|x| x >= 0.5) {
            counts.add("flags", "repeated_word_8gram_share_ge_0_5");
        }
        if row["format"]["hard_wrapped"] == true {
            counts.add("flags", &format!("hard_wrapped|{}", row["origin"].as_str().unwrap_or("")));
        }
        if row["format"]["list_lines"].as_u64().is_some_and(|x| x > 0) {
            counts.add("flags", &format!("has_list_lines|{}", row["origin"].as_str().unwrap_or("")));
        }
        docs.write(&row)?;
        row.as_object_mut().expect("object").remove("text");
        meta.write(&row)?;
    }
    let (doc_sha, doc_rows) = docs.finish()?;
    let (meta_sha, _) = meta.finish()?;
    let (excl_sha, excl_rows) = excl.finish()?;
    words.sort_unstable();
    let stats = if words.is_empty() { json!(null) } else {
        json!({"min": words[0], "median": words[words.len() / 2], "max": words[words.len() - 1], "mean": words.iter().sum::<usize>() as f64 / words.len() as f64})
    };
    Ok(json!({
        "candidates": n,
        "admitted": doc_rows,
        "excluded": excl_rows,
        "source_groups": group_names.len(),
        "admitted_source_groups": admitted_groups.len(),
        "source_groups_excluded_for_prior_conflict": prior_group.iter().filter(|&&b| b).count(),
        "prior_conflict_reasons_by_row": prior_reasons,
        "cross_group_shared_sequence_runs": cross_links,
        "multi_group_split_components": comp_size.values().filter(|&&s| s > 1).count(),
        "exclusion_reasons": excluded_reasons,
        "counts": counts.0,
        "word_counts": stats,
        "documents_sha256": doc_sha,
        "metadata_sha256": meta_sha,
        "exclusions_sha256": excl_sha,
        "protected_index_sha256": index.sha256,
        "protected_index_dir": o.index,
        "candidates_sha256": sha,
        "method": ADMIT_METHOD,
    }))
}

/// Rights decisions of every component, for the admission record.
pub fn rights_table() -> Value {
    json!({
        "dataset": {"name": "RAID", "license": "MIT", "evidence": ["https://github.com/liamdugan/raid/blob/main/LICENSE", DATASET_URL], "citation": CITATION},
        "human_sources": DOMAINS.iter().map(|d| json!({"raid_domain": d.raid, "english": d.english, "source": d.human_source, "url": d.human_source_url,
            "terms": d.human_terms, "license": d.human_license, "date": d.human_date, "date_basis": d.human_date_basis,
            "human_rows_admitted": d.human_excluded.is_none(), "excluded_reason": d.human_excluded,
            "generated_rows_admitted": d.english,
            "generated_rows_basis": if d.english { "prompts carry only the source title" } else { "not English or not prose" }})).collect::<Vec<_>>(),
        "generators": GENERATORS.iter().map(|g| json!({"raid_model": g.raid, "label": g.label, "checkpoint": g.checkpoint, "family": g.family,
            "access": g.access, "terms": g.terms, "terms_url": g.terms_url, "admitted": g.excluded.is_none(), "excluded_reason": g.excluded, "note": g.note})).collect::<Vec<_>>(),
        "attacks": ATTACKS.iter().map(|(n, t, e)| json!({"attack": n, "raid_mutation_rate": t, "final_text_editor": e,
            "human_rows": match *n { "paraphrase" => "admitted as model_only source-conditioned paraphrase (cohort raid-v1-human-paraphrase)",
                                     "synonym" => "excluded: model-substituted words without recorded spans", _ => "admitted as human_only with the attack flag" }})).collect::<Vec<_>>(),
        "cohorts": {
            COHORT_PAIRED: "domains with admitted human references (books, reddit, wiki): humans and their generations",
            COHORT_UNPAIRED: "generations of domains whose human references are excluded (abstracts, news, poetry, recipes, reviews); no paired human negatives in RAID, so weight with care against domain shortcuts",
            COHORT_PARAPHRASE: "DIPPER paraphrases of admitted human references (model_only, source-conditioned rewrite)",
        },
        "use": "Train only. RAID train/extra text is public with labels and never enters an internal evaluation split; the RAID test split is used only through the official leaderboard.",
    })
}

/// Write `admission.json`, the `admission/` records and `SHA256SUMS`.
pub fn finalize(pool: &Path, adapter_dir: &Path, admit: &Value, index_dir: &Path) -> Result<Value> {
    let adm = pool.join("admission");
    std::fs::copy(adapter_dir.join("report.json"), adm.join("adapter-report.json"))?;
    let adapter: Value = serde_json::from_slice(&std::fs::read(adapter_dir.join("report.json"))?)?;
    io::write_json(&adm.join("admit-report.json"), admit)?;
    io::write_json(&adm.join("rights.json"), &rights_table())?;
    let idx: Value = serde_json::from_slice(&std::fs::read(index_dir.join("index.json"))?)?;
    let index_summary = json!({"dir": index_dir, "index_json_sha256": admit["protected_index_sha256"], "registry_path": idx["registry_path"],
        "registry_sha256": idx["registry_sha256"], "files_scanned": idx["files_scanned"], "strings_indexed": idx["strings_indexed"],
        "probe_filter": idx["probe_filter"], "entries_without_files": idx["entries_without_files"], "built_utc": idx["built_utc"]});
    io::write_json(&adm.join("prior-index.json"), &index_summary)?;
    let summary = json!({
        "schema": "slop_ninja_a_pool_source_admission_summary_v1",
        "source": "raid-v1",
        "collection": COLLECTION,
        "date": io::now_utc().get(..10),
        "host": "build-host",
        "status": "admitted_to_candidate_pool_not_split_assigned",
        "intended_use": "train_only",
        "cohort_family": COHORT_FAMILY,
        "cohorts": [COHORT_PAIRED, COHORT_UNPAIRED, COHORT_PARAPHRASE],
        "rights": {"dataset_license": "MIT", "row_license": LICENSE, "summary": "see admission/rights.json; human rows only from book summaries (CC BY-SA 3.0), Reddit (Webis-TLDR-17) and Wikipedia (CC BY-SA 3.0); MPT-30B-Chat rows and the non-English or code domains are excluded",
                   "redistribution": "never; exact text stays in ignored storage"},
        "labels": {"human_only": "RAID model=human, attack not paraphrase or synonym", "model_only": "RAID generations (any attack) and DIPPER paraphrases of human rows",
                   "share_target": "0 for human_only, 1 for model_only; ai_byte_spans cover the whole text for model_only"},
        "grouping": "source_group = raid_source:<RAID source_id>; split_component unions groups linked by cross-group duplicates",
        "inputs": adapter["inputs"],
        "adapter_parameters": adapter["parameters"],
        "counts": {"units_seen": adapter["units_seen"], "adapter_candidates": adapter["candidates"], "adapter_rejections": adapter["rejection_reasons"],
                   "admitted": admit["admitted"], "excluded_at_admission": admit["exclusion_reasons"], "source_groups": admit["source_groups"],
                   "admitted_source_groups": admit["admitted_source_groups"], "source_groups_excluded_for_prior_conflict": admit["source_groups_excluded_for_prior_conflict"],
                   "by_axis": admit["counts"], "word_counts": admit["word_counts"]},
        "overlap": {"method": ADMIT_METHOD, "protected_index": index_summary,
                    "prior_conflict_policy": "a text or family conflict of any row excludes its whole RAID source group",
                    "within_pool_policy": "a row sharing a whole-text key or a folded 50-word sequence with an earlier-ranked row of another source group is excluded and the groups are linked into one split component; exact duplicates inside a group are excluded; attack variants inside a group are kept"},
        "detector_calls": 0, "pangram_calls": 0, "gpu_used": false, "external_text_services": 0,
        "limits": [
            "RAID labels are benchmark labels: human rows are pre-cutoff proxies, not authenticated unaided writing.",
            "Generated rows are title-conditioned 2023 generations; their decoding and attack settings follow RAID, not our planner.",
            "Attacked rows are a seeded sample; see adapter_parameters.",
            "The prior index is probe-filtered to unattacked candidates; attacked variants are excluded with their group when the group conflicts.",
            "Rows outside 150..=450 whitespace words are not admitted; the RAID test split includes shorter texts.",
            "Pretraining exposure for the encoder and comparator is unknown; RAID has been public since 2024.",
            "Admission is not a split assignment."
        ],
    });
    io::write_json(&pool.join("admission.json"), &summary)?;
    let mut sums = String::new();
    let mut files = vec![pool.join("documents.jsonl"), pool.join("metadata.jsonl"), pool.join("admission.json")];
    let mut extra: Vec<PathBuf> = std::fs::read_dir(&adm)?.filter_map(|e| e.ok().map(|e| e.path())).collect();
    extra.sort();
    files.extend(extra);
    for f in files {
        let (sha, _) = io::file_sha(&f)?;
        sums.push_str(&format!("{sha}  {}\n", f.strip_prefix(pool)?.display()));
    }
    let p = pool.join("SHA256SUMS");
    ensure!(!p.exists(), "SHA256SUMS exists");
    std::fs::write(&p, sums)?;
    Ok(summary)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all(s: &str) -> Vec<Vec<String>> {
        let mut r = CsvStream::new(std::io::BufReader::with_capacity(3, s.as_bytes()));
        let mut v = Vec::new();
        while let Some(x) = r.next_record().unwrap() {
            v.push(x);
        }
        v
    }

    #[test]
    fn streaming_csv_matches_rfc4180_across_tiny_buffers() {
        let v = all("\u{FEFF}a,b,c\r\n1,\"x, \"\"y\"\"\nz\",\r\n\"\",q,\"r\"\n");
        assert_eq!(v, vec![vec!["a", "b", "c"], vec!["1", "x, \"y\"\nz", ""], vec!["", "q", "r"]]);
        assert_eq!(all("a,b\n1,2"), vec![vec!["a", "b"], vec!["1", "2"]]);
        let mut r = CsvStream::new(std::io::BufReader::new("\"abc".as_bytes()));
        assert!(r.next_record().is_err());
        let mut r = CsvStream::new(std::io::BufReader::new("\"a\"b,c\n".as_bytes()));
        assert!(r.next_record().is_err());
        // Agrees with the in-memory reader on the same input.
        let s = "id,generation\nx,\"one\n\ntwo \"\"q\"\"\"\ny,plain\n";
        let mut t = crate::csvread::Reader::new(s);
        let mut w = Vec::new();
        while let Some(x) = t.next_record().unwrap() {
            w.push(x);
        }
        assert_eq!(all(s), w);
    }

    #[test]
    fn tables_cover_raid_and_gate_rights() {
        for m in ["gpt2", "gpt3", "chatgpt", "gpt4", "llama-chat", "mistral", "mistral-chat", "mpt", "mpt-chat", "cohere", "cohere-chat"] {
            assert!(generator(m).is_some(), "{m}");
        }
        assert!(generator("mpt-chat").unwrap().excluded.is_some());
        assert_eq!(GENERATORS.iter().filter(|g| g.excluded.is_some()).count(), 1);
        let admitted: Vec<_> = DOMAINS.iter().filter(|d| d.human_excluded.is_none()).map(|d| d.raid).collect();
        assert_eq!(admitted, ["books", "reddit", "wiki"]);
        for d in DOMAINS.iter().filter(|d| d.human_excluded.is_none()) {
            assert!(record::admitted_license(d.human_license.unwrap()), "{}", d.raid);
            assert!(record::before_cutoff(d.human_date.unwrap()), "{}", d.raid);
        }
        assert_eq!(ATTACKS.len(), 12);
    }

    #[test]
    fn fit_keeps_whole_units_and_crops_at_paragraphs() {
        let p = |n: usize, w: &str| vec![w; n].join(" ") + ".";
        let short = p(100, "a");
        assert_eq!(fit(&short), Err("word_count_below_150"));
        let whole = format!("  {}\n", p(200, "b"));
        let (t, span, complete) = fit(&whole).unwrap();
        assert_eq!((t.as_str(), span, complete), (p(200, "b").as_str(), None, true));
        let long = format!("{}\n\n{}\n \n{}", p(120, "c"), p(120, "d"), p(300, "e"));
        let (t, span, complete) = fit(&long).unwrap();
        assert_eq!(t, format!("{}\n\n{}", p(120, "c"), p(120, "d")));
        assert_eq!((span, complete), (Some([0, 2]), false));
        assert_eq!(fit(&p(500, "f")), Err("no_whole_paragraph_run_within_150_450"));
    }

    #[test]
    fn flags_detect_loops_wraps_and_lists() {
        let loop_text = vec!["the same words again and again here now"; 20].join(" ");
        assert!(repeated_8gram_share(&loop_text) > 0.8);
        assert!(repeated_8gram_share("one two three four five six seven eight nine ten") == 0.0);
        let wrapped = (0..6).map(|_| "x".repeat(70)).collect::<Vec<_>>().join("\n");
        assert!(hard_wrapped(&wrapped));
        assert!(!hard_wrapped("a short line\nanother"));
        assert_eq!(format_flags("Intro:\n1. eggs\n2) milk\n- salt\nplain")["list_lines"], 3);
    }
}
