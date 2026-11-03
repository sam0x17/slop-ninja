//! Pre-mainnet era gates on the Common Crawl-parented mock batch
//! (whitepaper 07-rewards "Human stock").
//!
//! 1. **Era probe, parents against mirrors.** The hashed n-gram era probe
//!    (`era_probe`, same design and hyperparameters) is retrained on pilot
//!    humans (2013–2016 against 2020–2022), excluding every source group in the
//!    batch. The gate is the AUC of P(late) for topic- and date-conditioned
//!    mirrors against their own parents: at most 0.60. The topic-only and
//!    unconditioned arms are reported beside it.
//! 2. **Era-only and format-only adversaries** on the 20/55/25 mock cohort.
//!    Each is a cross-fitted logistic model (group folds) on a fixed feature
//!    set, in a linear and a quantile-binned form with a small ridge grid;
//!    the reported gain is the best of those. Gain is the Brier improvement
//!    over the constant prior (the training folds' base rate), as a share of
//!    the reference's headroom B0. Gate: at most 5% each. A secondary,
//!    incremental form adds the features to the reference's own logit.
//!
//! **Gate definitions** are versioned (`GATE_DEFS_V1`, `GATE_DEFS_V2`); every
//! run computes both verdicts and names the selected one:
//! - v1 (pre-registered in era-v1): the probe AUC at most 0.60, the era-only
//!   adversary at most 5% and the format-only adversary (all 49 format
//!   features) at most 5%.
//! - v2 (the default): the era gate is the era-only adversary at most 5%,
//!   with the probe AUC reported as a diagnostic; the format gate is the
//!   layout and typography adversary (the format features without the
//!   character-class block) at most 5%, with the character-class block
//!   (case, digits, non-ASCII, symbols, emoji, punctuation density, control
//!   characters) reported as a monitored diagnostic.
//!
//! Length diagnostics: the length-only adversary (log words and log
//! characters) and its two parts, word count alone and mean word length
//! (log characters per word), so a length rule can be read apart from a
//! lexical one.
//!
//! 3. **B0, sigma, delta and w** on the same cohort: B0 is the reference's
//!    mean Brier loss (self-weighting); sigma is the pooled paired standard
//!    error of dq for the retrain seeds against the reference at 1k, 2k and
//!    4k cases (stratified by domain x origin, analytic and bootstrap);
//!    delta = max(0.01, 2 sigma) and w = 2 sigma B0 (07-rewards).
//!
//! The output holds aggregate statistics only. No detector output labels,
//! filters or removes a document.
use crate::era_batch::{self, Doc, HUMAN, MIRROR_DATED, MIRROR_PLAIN, MIRROR_TOPIC, Parent};
use crate::era_probe;
use crate::measure::{self, EARLY, LATE, Rng, auc, crawl_year, file_sha256, percentile, read_jsonl};
use crate::store;
use anyhow::{Context, Result, ensure};
use grammar_a_pools::splits::rank;
use rayon::prelude::*;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    path::PathBuf,
};

pub const GATES_SCHEMA: &str = "slop_ninja_cc_era_gates_v2";
pub const ERA_FEATURES_ID: &str = "cc_era_features_v1";
pub const FORMAT_FEATURES_ID: &str = "cc_format_features_v1";
pub const AUC_GATE: f64 = 0.60;
pub const ADVERSARY_GATE: f64 = 0.05;
pub const GATE_DEFS_V1: &str = "cc_era_gate_defs_v1";
pub const GATE_DEFS_V2: &str = "cc_era_gate_defs_v2";
pub const DEFAULT_GATE_DEFS: &str = GATE_DEFS_V2;
/// Launch batch mix (human-only, mixed, model-only).
pub const TARGET_MIX: [f64; 3] = [0.20, 0.55, 0.25];

/// Recency lexicon v1: dated events, products, platforms and technologies
/// (token sequences after `era_probe::tokens`). Generic style words are
/// excluded so the era adversary does not borrow model style.
pub const RECENCY_LEXICON: [(&str, &[&str]); 7] = [
    ("pandemic", &["covid", "coronavirus", "pandemic", "pandemics", "lockdown", "lockdowns", "quarantine", "social distancing", "omicron", "vaccination", "vaccinated", "zoom", "post pandemic"]),
    ("ai", &["ai", "chatgpt", "gpt", "llm", "llms", "chatbot", "chatbots", "generative", "machine learning", "artificial intelligence", "deep learning", "neural network", "neural networks", "openai"]),
    ("platforms", &["tiktok", "instagram", "snapchat", "whatsapp", "netflix", "spotify", "uber", "airbnb", "podcast", "podcasts", "influencer", "influencers", "streaming", "smartphone", "smartphones", "app", "apps", "airpods", "alexa"]),
    ("crypto", &["crypto", "cryptocurrency", "cryptocurrencies", "bitcoin", "blockchain", "nft", "nfts", "ethereum", "metaverse", "web3"]),
    ("economy_climate", &["remote work", "hybrid work", "work from home", "inflation", "supply chain", "supply chains", "gig economy", "net zero", "climate crisis", "carbon footprint", "electric vehicle", "electric vehicles", "evs"]),
    ("politics", &["ukraine", "trump", "biden", "brexit", "putin", "harris", "pelosi"]),
    ("older", &["blackberry", "myspace", "ipod", "dvd", "dvds", "netbook", "flash player", "windows xp", "windows 7", "obama", "romney", "mp3", "blogging", "web 2"]),
];

fn word_count(text: &str) -> f64 {
    (text.split_whitespace().count().max(1)) as f64
}

/// Era-only features: the probe's raw and digit-masked logits, explicit year
/// mentions and the recency lexicon. Returns (names, values).
pub fn era_features(text: &str, logit_raw: f64, logit_masked: f64) -> (Vec<String>, Vec<f64>) {
    let toks = era_probe::tokens(text, false);
    let w = word_count(text);
    let mut names: Vec<String> = vec!["probe_logit_raw".into(), "probe_logit_masked".into()];
    let mut v = vec![logit_raw, logit_masked];
    let years: Vec<u16> = toks.iter().filter(|t| t.len() == 4 && t.chars().all(|c| c.is_ascii_digit())).filter_map(|t| t.parse().ok()).filter(|y| (1900..=2099).contains(y)).collect();
    let n = years.len() as f64;
    let frac = |f: &dyn Fn(u16) -> bool| if years.is_empty() { 0.0 } else { years.iter().filter(|&&y| f(y)).count() as f64 / n };
    for (k, x) in [
        ("years_per_100_words", n * 100.0 / w),
        ("any_year", (n > 0.0) as u8 as f64),
        ("max_year_minus_2010_over_10", years.iter().max().map_or(0.0, |&y| (y as f64 - 2010.0) / 10.0)),
        ("share_years_le_2012", frac(&|y| y <= 2012)),
        ("share_years_2013_2016", frac(&|y| (2013..=2016).contains(&y))),
        ("share_years_2017_2019", frac(&|y| (2017..=2019).contains(&y))),
        ("share_years_2020_2022", frac(&|y| (2020..=2022).contains(&y))),
        ("share_years_ge_2023", frac(&|y| y >= 2023)),
        ("any_year_ge_2023", years.iter().any(|&y| y >= 2023) as u8 as f64),
    ] {
        names.push(k.into());
        v.push(x);
    }
    for (cat, terms) in RECENCY_LEXICON {
        let mut hits = 0usize;
        for t in terms {
            let seq: Vec<&str> = t.split(' ').collect();
            hits += toks.windows(seq.len()).filter(|win| win.iter().zip(&seq).all(|(a, b)| a == b)).count();
        }
        names.push(format!("lexicon_{cat}_per_100_words"));
        v.push(hits as f64 * 100.0 / w);
        names.push(format!("lexicon_{cat}_any"));
        v.push((hits > 0) as u8 as f64);
    }
    (names, v)
}

/// The "Unicode classes" block of `format_features` (case, digits, non-ASCII,
/// symbols, emoji, punctuation density, control characters). Reported as a
/// diagnostic split of the format gate; whether these count as format is
/// left open.
pub const CHAR_CLASS_FEATURES: [&str; 8] = [
    "share_non_ascii", "share_non_ascii_letters", "share_upper_among_letters", "share_digits", "share_ascii_punct", "share_non_ascii_symbols",
    "emoji_or_pictograph_per_1k", "control_chars_per_1k",
];

/// Descriptive per-origin summary of a few format features (means over documents).
fn format_summary(names: &[String], rows: &[Vec<f64>], origins: &[&str]) -> Value {
    let col = |k: &str| names.iter().position(|n| n == k).expect("format feature");
    let (ts, sb, cs, cd, sc, aq, pb, wpl, un, em, el, td) = (
        col("trailing_space_lines"), col("single_newline_breaks_per_line"), col("curly_single_quote_per_1k"), col("curly_double_quote_per_1k"),
        col("share_curly_among_quotes"), col("any_quote"), col("paragraphs_blank_separated"), col("mean_words_per_line"),
        col("share_lines_unterminated"), col("em_dash_per_1k"), col("ellipsis_char_per_1k"), col("three_dots_per_1k"),
    );
    let mut out = BTreeMap::new();
    for o in ["human_only", "mixed", "model_only"] {
        let idx: Vec<usize> = (0..rows.len()).filter(|&i| origins[i] == o).collect();
        let n = idx.len().max(1) as f64;
        let share = |f: &dyn Fn(&Vec<f64>) -> bool| idx.iter().filter(|&&i| f(&rows[i])).count() as f64 / n;
        let avg = |c: usize| idx.iter().map(|&i| rows[i][c]).sum::<f64>() / n;
        let quoted: Vec<usize> = idx.iter().copied().filter(|&i| rows[i][aq] > 0.0).collect();
        out.insert(o.to_string(), json!({"documents": idx.len(),
            "share_with_trailing_space_lines": share(&|r| r[ts] > 0.0), "share_with_single_newline_breaks": share(&|r| r[sb] > 0.0),
            "share_with_curly_quotes": share(&|r| r[cs] + r[cd] > 0.0),
            "mean_curly_share_among_quoted_documents": quoted.iter().map(|&i| rows[i][sc]).sum::<f64>() / quoted.len().max(1) as f64,
            "share_with_em_dash": share(&|r| r[em] > 0.0), "share_with_ellipsis_char": share(&|r| r[el] > 0.0), "share_with_three_dots": share(&|r| r[td] > 0.0),
            "mean_paragraphs": avg(pb), "mean_words_per_line": avg(wpl), "mean_share_lines_unterminated": avg(un)}));
    }
    json!(out)
}

const NAV_TERMS: [&str; 22] = [
    "home", "share", "tweet", "read more", "click here", "posted by", "comments", "reply", "log in", "login", "sign up", "subscribe",
    "copyright", "all rights reserved", "privacy policy", "terms of use", "next", "previous", "menu", "related posts", "tags", "advertisement",
];

/// Format-only features: quote and dash codepoints, whitespace and line
/// patterns, Unicode classes, residual navigation and markup. Content words
/// and style are excluded. Returns (names, values).
pub fn format_features(text: &str) -> (Vec<String>, Vec<f64>) {
    let chars: Vec<char> = text.chars().collect();
    let nc = chars.len().max(1) as f64;
    let per_k = |n: usize| n as f64 * 1000.0 / nc;
    let count = |f: &dyn Fn(char) -> bool| chars.iter().filter(|&&c| f(c)).count();
    let mut names: Vec<String> = Vec::new();
    let mut v = Vec::new();
    let mut push = |k: &str, x: f64| {
        names.push(k.to_string());
        v.push(x);
    };
    // Quotes and apostrophes.
    let ascii_sq = count(&|c| c == '\'');
    let ascii_dq = count(&|c| c == '"');
    let curly_s = count(&|c| c == '\u{2018}' || c == '\u{2019}');
    let curly_d = count(&|c| c == '\u{201C}' || c == '\u{201D}');
    let other_q = count(&|c| matches!(c, '`' | '\u{00B4}' | '\u{00AB}' | '\u{00BB}' | '\u{201A}' | '\u{201E}' | '\u{2032}' | '\u{2033}'));
    push("ascii_single_quote_per_1k", per_k(ascii_sq));
    push("ascii_double_quote_per_1k", per_k(ascii_dq));
    push("curly_single_quote_per_1k", per_k(curly_s));
    push("curly_double_quote_per_1k", per_k(curly_d));
    push("other_quote_per_1k", per_k(other_q));
    let all_q = ascii_sq + ascii_dq + curly_s + curly_d;
    push("share_curly_among_quotes", if all_q == 0 { 0.0 } else { (curly_s + curly_d) as f64 / all_q as f64 });
    push("any_quote", (all_q > 0) as u8 as f64);
    // Dashes and ellipses.
    push("hyphen_minus_per_1k", per_k(count(&|c| c == '-')));
    push("en_dash_per_1k", per_k(count(&|c| c == '\u{2013}')));
    push("em_dash_per_1k", per_k(count(&|c| c == '\u{2014}' || c == '\u{2015}')));
    push("other_dash_per_1k", per_k(count(&|c| matches!(c, '\u{2010}' | '\u{2011}' | '\u{2012}' | '\u{2212}'))));
    push("spaced_hyphen_per_1k", per_k(text.matches(" - ").count()));
    push("double_hyphen_per_1k", per_k(text.matches("--").count()));
    push("ellipsis_char_per_1k", per_k(count(&|c| c == '\u{2026}')));
    push("three_dots_per_1k", per_k(text.matches("...").count()));
    // Whitespace and lines.
    let lines: Vec<&str> = text.split('\n').collect();
    let nonblank: Vec<&str> = lines.iter().copied().filter(|l| !l.trim().is_empty()).collect();
    let nl = nonblank.len().max(1) as f64;
    let paras = text.split("\n\n").filter(|p| !p.trim().is_empty()).count();
    let wpl: Vec<f64> = nonblank.iter().map(|l| l.split_whitespace().count() as f64).collect();
    let terminal = |l: &str| l.trim_end().ends_with(['.', '!', '?', '"', '\u{201D}', '\'', '\u{2019}', ')', ':', ';', '\u{2026}']);
    push("lines", nonblank.len() as f64);
    push("ln_lines", (nonblank.len() as f64 + 1.0).ln());
    push("paragraphs_blank_separated", paras as f64);
    push("blank_lines_per_line", lines.iter().filter(|l| l.trim().is_empty()).count() as f64 / nl);
    push("single_newline_breaks_per_line", text.match_indices('\n').filter(|(i, _)| !text[i + 1..].starts_with('\n') && (*i == 0 || !text[..*i].ends_with('\n'))).count() as f64 / nl);
    push("mean_words_per_line", wpl.iter().sum::<f64>() / nl);
    push("max_words_per_line", wpl.iter().copied().fold(0.0, f64::max));
    push("share_lines_unterminated", nonblank.iter().filter(|l| !terminal(l)).count() as f64 / nl);
    push("double_space_per_1k", per_k(text.matches("  ").count()));
    push("trailing_space_lines", lines.iter().filter(|l| l.ends_with(' ') || l.ends_with('\t')).count() as f64);
    push("leading_space_lines", lines.iter().filter(|l| l.starts_with(' ') || l.starts_with('\t')).count() as f64);
    push("tab_per_1k", per_k(count(&|c| c == '\t')));
    push("nbsp_per_1k", per_k(count(&|c| c == '\u{00A0}')));
    push("unicode_space_per_1k", per_k(count(&|c| matches!(c, '\u{2000}'..='\u{200A}' | '\u{202F}' | '\u{205F}' | '\u{3000}'))));
    push("zero_width_or_bom_per_1k", per_k(count(&|c| matches!(c, '\u{200B}'..='\u{200D}' | '\u{2060}' | '\u{FEFF}' | '\u{00AD}'))));
    push("carriage_return_per_1k", per_k(count(&|c| c == '\r')));
    // Unicode classes.
    let letters = count(&|c| c.is_alphabetic()).max(1) as f64;
    push("share_non_ascii", count(&|c| !c.is_ascii()) as f64 / nc);
    push("share_non_ascii_letters", count(&|c| c.is_alphabetic() && !c.is_ascii()) as f64 / letters);
    push("share_upper_among_letters", count(&|c| c.is_uppercase()) as f64 / letters);
    push("share_digits", count(&|c| c.is_ascii_digit()) as f64 / nc);
    push("share_ascii_punct", count(&|c| c.is_ascii_punctuation()) as f64 / nc);
    push("share_non_ascii_symbols", count(&|c| !c.is_ascii() && !c.is_alphanumeric() && !c.is_whitespace()) as f64 / nc);
    push("emoji_or_pictograph_per_1k", per_k(count(&|c| matches!(c, '\u{1F300}'..='\u{1FAFF}' | '\u{2600}'..='\u{27BF}'))));
    push("control_chars_per_1k", per_k(count(&|c| c.is_control() && c != '\n' && c != '\t' && c != '\r')));
    // Residual navigation and markup.
    let lower = text.to_lowercase();
    let ltoks = era_probe::tokens(text, false);
    let mut nav = 0usize;
    for t in NAV_TERMS {
        let seq: Vec<&str> = t.split(' ').collect();
        nav += ltoks.windows(seq.len()).filter(|win| win.iter().zip(&seq).all(|(a, b)| a == b)).count();
    }
    push("nav_terms_per_100_words", nav as f64 * 100.0 / word_count(text));
    push("short_unterminated_lines", nonblank.iter().filter(|l| l.split_whitespace().count() <= 4 && !terminal(l)).count() as f64);
    push("separator_lines", nonblank.iter().filter(|l| l.contains(" | ") || l.contains('\u{00BB}') || l.contains('\u{203A}') || l.contains(" / ")).count() as f64);
    push("url_like_per_1k", per_k(lower.matches("http").count() + lower.matches("www.").count()));
    push("email_like_per_1k", per_k(text.split_whitespace().filter(|w| w.contains('@') && w.contains('.')).count()));
    push("at_or_hash_tokens_per_1k", per_k(text.split_whitespace().filter(|w| w.starts_with('@') || w.starts_with('#')).count()));
    push("copyright_sign_per_1k", per_k(count(&|c| c == '\u{00A9}' || c == '\u{00AE}' || c == '\u{2122}')));
    push("markdown_heading_lines", nonblank.iter().filter(|l| l.trim_start().starts_with('#')).count() as f64);
    push("bullet_lines", nonblank.iter().filter(|l| { let t = l.trim_start(); t.starts_with("- ") || t.starts_with("* ") || t.starts_with('\u{2022}') }).count() as f64);
    push("numbered_lines", nonblank.iter().filter(|l| { let t = l.trim_start(); let d: String = t.chars().take_while(|c| c.is_ascii_digit()).collect(); !d.is_empty() && t[d.len()..].starts_with(['.', ')']) }).count() as f64);
    push("bold_or_underscore_markup_per_1k", per_k(text.matches("**").count() + text.matches("__").count()));
    push("brackets_per_1k", per_k(count(&|c| matches!(c, '[' | ']' | '{' | '}' | '<' | '>'))));
    (names, v)
}


pub const NORMALISER_ID: &str = "cc_era_layout_norm_v0";

/// Candidate layout normaliser v0 (a mitigation under evaluation, not a
/// protocol rule): typographic quotes, primes and guillemets to ASCII quotes;
/// Unicode hyphens and dashes to '-'; the ellipsis character to "..."; NBSP,
/// Unicode spaces and tabs to a space; zero-width characters, soft hyphens,
/// BOM and CR removed; each line trimmed with inner space runs collapsed; blank
/// lines dropped; every remaining line becomes its own paragraph ("\n\n").
pub fn normalise_v0(t: &str) -> String {
    let mut s = String::with_capacity(t.len());
    for c in t.chars() {
        match c {
            '\u{2018}' | '\u{2019}' | '\u{201A}' | '\u{201B}' | '\u{2032}' | '`' | '\u{00B4}' => s.push('\''),
            '\u{201C}' | '\u{201D}' | '\u{201E}' | '\u{201F}' | '\u{2033}' | '\u{00AB}' | '\u{00BB}' => s.push('"'),
            '\u{2010}'..='\u{2015}' | '\u{2212}' => s.push('-'),
            '\u{2026}' => s.push_str("..."),
            '\u{00A0}' | '\u{2000}'..='\u{200A}' | '\u{202F}' | '\u{205F}' | '\u{3000}' | '\t' => s.push(' '),
            '\u{200B}'..='\u{200D}' | '\u{2060}' | '\u{FEFF}' | '\u{00AD}' | '\r' => {}
            c => s.push(c),
        }
    }
    let lines: Vec<String> = s.split('\n').map(|l| l.split(' ').filter(|w| !w.is_empty()).collect::<Vec<_>>().join(" ")).filter(|l| !l.is_empty()).collect();
    lines.join("\n\n")
}

pub const FLAT_NORMALISER_ID: &str = "cc_era_flat_norm_v0";

/// Candidate flat normaliser v0: layout normaliser v0, then every whitespace
/// run (line and paragraph breaks included) becomes one space.
pub fn flatten_v0(t: &str) -> String {
    normalise_v0(t).split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Writes a normalised copy of an assembled cohort (text and digest replaced,
/// AI byte spans cleared) and its A request shards. `flat` selects the flat
/// normaliser instead of the layout one.
pub fn normalise_cohort(cohort: &std::path::Path, out: &std::path::Path, shards: usize, flat: bool) -> Result<Value> {
    let mut docs: Vec<Doc> = store::rows_plain(cohort)?;
    for d in docs.iter_mut() {
        d.text = if flat { flatten_v0(&d.text) } else { normalise_v0(&d.text) };
        d.text_sha256 = measure::sha256_hex(d.text.as_bytes());
        d.words = d.text.split_whitespace().count();
        d.ai_spans.clear();
    }
    std::fs::create_dir_all(out.join("requests"))?;
    let sha = measure::write_jsonl(&out.join("cohort.jsonl"), &docs)?;
    let mut shard_sha = Vec::new();
    for s in 0..shards {
        let rows: Vec<Value> = docs.iter().enumerate().filter(|(i, _)| i % shards == s).map(|(_, d)| json!({"id":d.id,"text":d.text})).collect();
        shard_sha.push(measure::write_jsonl(&out.join("requests").join(format!("shard-{s:02}.jsonl")), &rows)?);
    }
    let rec = json!({"schema":"slop_ninja_cc_era_normalised_v1","normaliser":if flat { FLAT_NORMALISER_ID } else { NORMALISER_ID },"source":cohort,"source_sha256":file_sha256(cohort)?,
        "docs":docs.len(),"cohort_sha256":sha,"request_shards":shard_sha});
    store::write_json(&out.join("NORMALISED.json"), &rec)?;
    Ok(rec)
}

/// Length-only diagnostic features (not part of either gate).
pub fn length_features(text: &str) -> (Vec<String>, Vec<f64>) {
    (vec!["ln_words".into(), "ln_chars".into()], vec![word_count(text).ln(), (text.chars().count().max(1) as f64).ln()])
}

/// The word-count part of the length diagnostic.
pub fn word_count_features(text: &str) -> (Vec<String>, Vec<f64>) {
    (vec!["ln_words".into()], vec![word_count(text).ln()])
}

/// The mean-word-length part of the length diagnostic: log characters per
/// whitespace word (a lexical property, not document length).
pub fn mean_word_length_features(text: &str) -> (Vec<String>, Vec<f64>) {
    (vec!["ln_chars_per_word".into()], vec![(text.chars().count().max(1) as f64 / word_count(text)).ln()])
}

// ------------------------------------------------------------------ logistic

/// Cholesky solve of a symmetric positive-definite system (row-major `a`, p x p).
fn solve_spd(a: &mut [f64], b: &mut [f64], p: usize) -> Result<()> {
    for j in 0..p {
        let mut d = a[j * p + j];
        for k in 0..j {
            d -= a[j * p + k] * a[j * p + k];
        }
        ensure!(d > 1e-300, "matrix not positive definite");
        let d = d.sqrt();
        a[j * p + j] = d;
        for i in j + 1..p {
            let mut s = a[i * p + j];
            for k in 0..j {
                s -= a[i * p + k] * a[j * p + k];
            }
            a[i * p + j] = s / d;
        }
    }
    for i in 0..p {
        let mut s = b[i];
        for k in 0..i {
            s -= a[i * p + k] * b[k];
        }
        b[i] = s / a[i * p + i];
    }
    for i in (0..p).rev() {
        let mut s = b[i];
        for k in i + 1..p {
            s -= a[k * p + i] * b[k];
        }
        b[i] = s / a[i * p + i];
    }
    Ok(())
}

fn sigmoid(z: f64) -> f64 {
    1.0 / (1.0 + (-z).exp())
}

/// Ridge logistic regression by Newton steps. `x` rows carry a leading 1
/// (the unpenalised intercept). Returns coefficients.
pub fn fit_logistic(x: &[Vec<f64>], y: &[f64], wt: &[f64], lambda: f64) -> Result<Vec<f64>> {
    let p = x[0].len();
    let mut beta = vec![0.0; p];
    let mean = y.iter().zip(wt).map(|(a, b)| a * b).sum::<f64>() / wt.iter().sum::<f64>();
    beta[0] = (mean.clamp(1e-6, 1.0 - 1e-6) / (1.0 - mean.clamp(1e-6, 1.0 - 1e-6))).ln();
    for _ in 0..100 {
        let mut h = vec![0.0; p * p];
        let mut g = vec![0.0; p];
        for ((xi, &yi), &wi) in x.iter().zip(y).zip(wt) {
            let z: f64 = xi.iter().zip(&beta).map(|(a, b)| a * b).sum();
            let mu = sigmoid(z);
            let wgt = wi * (mu * (1.0 - mu)).max(1e-10);
            for j in 0..p {
                if xi[j] == 0.0 {
                    continue;
                }
                g[j] += wi * (yi - mu) * xi[j];
                let wj = wgt * xi[j];
                for k in 0..=j {
                    h[j * p + k] += wj * xi[k];
                }
            }
        }
        for j in 0..p {
            for k in 0..j {
                h[k * p + j] = h[j * p + k];
            }
        }
        for j in 1..p {
            g[j] -= lambda * beta[j];
            h[j * p + j] += lambda;
        }
        h[0] += 1e-9;
        solve_spd(&mut h, &mut g, p)?;
        let step = g.iter().fold(0.0f64, |m, x| m.max(x.abs()));
        // Damped Newton: cap the largest coordinate step at 5.
        let scale = if step > 5.0 { 5.0 / step } else { 1.0 };
        for j in 0..p {
            beta[j] += scale * g[j];
        }
        if step < 1e-9 {
            break;
        }
    }
    Ok(beta)
}

/// Design rows from raw features: `linear` standardises with training
/// moments; `binned` one-hot codes training-quantile bins (octiles, merged
/// when cut points repeat), first bin as reference.
#[derive(Clone)]
struct Design {
    binned: bool,
    mean: Vec<f64>,
    sd: Vec<f64>,
    cuts: Vec<Vec<f64>>,
}

impl Design {
    fn fit(train: &[&Vec<f64>], binned: bool) -> Design {
        let k = train[0].len();
        let n = train.len() as f64;
        let mut mean = vec![0.0; k];
        let mut sd = vec![0.0; k];
        let mut cuts = vec![Vec::new(); k];
        for j in 0..k {
            let col: Vec<f64> = train.iter().map(|r| r[j]).collect();
            let m = col.iter().sum::<f64>() / n;
            let s = (col.iter().map(|x| (x - m) * (x - m)).sum::<f64>() / n).sqrt();
            mean[j] = m;
            sd[j] = if s > 1e-12 { s } else { 0.0 };
            if binned {
                let mut c = col.clone();
                let mut cs: Vec<f64> = (1..8).map(|q| percentile(&mut c, q as f64 / 8.0)).collect();
                cs.dedup_by(|a, b| (*a - *b).abs() < 1e-12);
                let lo = col.iter().copied().fold(f64::INFINITY, f64::min);
                cs.retain(|&x| x > lo);
                cuts[j] = cs;
            }
        }
        Design { binned, mean, sd, cuts }
    }
    fn row(&self, r: &[f64]) -> Vec<f64> {
        let mut out = vec![1.0];
        for (j, &x) in r.iter().enumerate() {
            if self.binned {
                for &c in &self.cuts[j] {
                    out.push((x >= c) as u8 as f64);
                }
            } else if self.sd[j] > 0.0 {
                out.push((x - self.mean[j]) / self.sd[j]);
            } else {
                out.push(0.0);
            }
        }
        out
    }
}

pub const LAMBDAS: [f64; 4] = [0.1, 1.0, 10.0, 100.0];

/// Cross-fitted predictions of one feature set: fold of each document,
/// returns per-document predictions for each (form, lambda) and the prior.
/// Cross-fitted predictions per (form, lambda) configuration.
type Preds = BTreeMap<String, Vec<f64>>;
/// One fold: its index, the test predictions of each configuration and the prior.
type FoldOut = (usize, Vec<(usize, Vec<f64>)>, f64);

pub(crate) fn cross_fit(feats: &[Vec<f64>], y: &[f64], w: &[f64], folds: &[usize], k: usize) -> Result<(Preds, Vec<f64>)> {
    let n = feats.len();
    let mut prior = vec![0.0; n];
    let configs: Vec<(bool, f64)> = [false, true].iter().flat_map(|&b| LAMBDAS.iter().map(move |&l| (b, l))).collect();
    let per_fold: Vec<Result<FoldOut>> = (0..k)
        .into_par_iter()
        .map(|f| -> Result<FoldOut> {
            let tr: Vec<usize> = (0..n).filter(|&i| folds[i] != f).collect();
            let te: Vec<usize> = (0..n).filter(|&i| folds[i] == f).collect();
            let base = tr.iter().map(|&i| w[i] * y[i]).sum::<f64>() / tr.iter().map(|&i| w[i]).sum::<f64>();
            let ytr: Vec<f64> = tr.iter().map(|&i| y[i]).collect();
            let wtr: Vec<f64> = tr.iter().map(|&i| w[i]).collect();
            let mut outs = Vec::new();
            for (ci, &(binned, lambda)) in configs.iter().enumerate() {
                let refs: Vec<&Vec<f64>> = tr.iter().map(|&i| &feats[i]).collect();
                let d = Design::fit(&refs, binned);
                let xtr: Vec<Vec<f64>> = tr.iter().map(|&i| d.row(&feats[i])).collect();
                let beta = fit_logistic(&xtr, &ytr, &wtr, lambda)?;
                let pred: Vec<f64> = te.iter().map(|&i| sigmoid(d.row(&feats[i]).iter().zip(&beta).map(|(a, b)| a * b).sum())).collect();
                outs.push((ci, pred));
            }
            Ok((f, outs, base))
        })
        .collect();
    let mut preds: BTreeMap<String, Vec<f64>> = BTreeMap::new();
    for r in per_fold {
        let (f, outs, base) = r?;
        let te: Vec<usize> = (0..n).filter(|&i| folds[i] == f).collect();
        for &i in &te {
            prior[i] = base;
        }
        for (ci, pred) in outs {
            let (binned, lambda) = configs[ci];
            let key = format!("{}_l{lambda}", if binned { "binned" } else { "linear" });
            let v = preds.entry(key).or_insert_with(|| vec![0.0; n]);
            for (k2, &i) in te.iter().enumerate() {
                v[i] = pred[k2];
            }
        }
    }
    Ok((preds, prior))
}

/// Weighted mean Brier loss over `idx` (weights restore the target origin mix).
fn brier(p: &[f64], y: &[f64], w: &[f64], idx: &[usize]) -> f64 {
    let sw: f64 = idx.iter().map(|&i| w[i]).sum();
    idx.iter().map(|&i| w[i] * (p[i] - y[i]).powi(2)).sum::<f64>() / sw
}

fn logit(p: f64) -> f64 {
    let p = p.clamp(1e-6, 1.0 - 1e-6);
    (p / (1.0 - p)).ln()
}

/// (name, feature names, per-document feature rows)
type FeatureSet = (&'static str, Vec<String>, Vec<Vec<f64>>);

pub struct GatesArgs {
    pub cohort: PathBuf,
    pub batch: PathBuf,
    pub pool: PathBuf,
    /// NAME=predictions.jsonl; the first is the reference R0.
    pub preds: Vec<(String, PathBuf)>,
    pub threshold: f64,
    pub out: PathBuf,
    pub probe_seed: String,
    pub seed: String,
    pub draws: usize,
    pub folds: usize,
    pub cv_repeats: usize,
    pub per_domain_class: usize,
    /// Selected gate definitions (`GATE_DEFS_V1` or `GATE_DEFS_V2`).
    pub gate_defs: String,
}

pub(crate) struct Probe {
    raw: era_probe::Model,
    masked: era_probe::Model,
    dims: u32,
}

impl Probe {
    pub(crate) fn logits(&self, t: &str) -> (f64, f64) {
        (self.raw.logit(&era_probe::features(t, false, self.dims)), self.masked.logit(&era_probe::features(t, true, self.dims)))
    }
}

/// The era probe of `era_probe::run`, retrained without the batch's source groups.
pub(crate) fn train_probe(a: &GatesArgs, exclude_groups: &HashSet<String>) -> Result<(Probe, Value)> {
    let dims = 20u32;
    let pool = measure::read_pool(&a.pool)?;
    let mut by: BTreeMap<(String, bool), Vec<(u64, usize)>> = BTreeMap::new();
    let mut excluded = 0usize;
    for (i, r) in pool.iter().enumerate() {
        if exclude_groups.contains(&r.group) {
            excluded += 1;
            continue;
        }
        let y = crawl_year(&r.crawl)?;
        let class = if (LATE.0..=LATE.1).contains(&y) { true } else if (EARLY.0..=EARLY.1).contains(&y) { false } else { continue };
        by.entry((r.domain.clone(), class)).or_default().push((rank(&a.probe_seed, era_probe::SELECT_SCOPE, &r.id), i));
    }
    let mut domains: Vec<String> = by.keys().map(|k| k.0.clone()).collect();
    domains.dedup();
    let mut tr_x = (Vec::new(), Vec::new());
    let mut tr_y = Vec::new();
    let mut te: Vec<(usize, bool)> = Vec::new();
    for d in &domains {
        let n = a.per_domain_class.min(by.get(&(d.clone(), false)).map_or(0, |v| v.len())).min(by.get(&(d.clone(), true)).map_or(0, |v| v.len()));
        for class in [false, true] {
            let mut v = by[&(d.clone(), class)].clone();
            v.sort();
            for &(_, i) in v.iter().take(n) {
                let r = &pool[i];
                if rank(&a.probe_seed, era_probe::SPLIT_SCOPE, &r.group).is_multiple_of(5) {
                    te.push((i, class));
                } else {
                    tr_x.0.push(era_probe::features(r.scored_text().0, false, dims));
                    tr_x.1.push(era_probe::features(r.scored_text().0, true, dims));
                    tr_y.push(class as u8 as f64);
                }
            }
        }
    }
    let raw = era_probe::train(&tr_x.0, &tr_y, dims, 5, 0.2, 1e-6, rank(&a.probe_seed, "cc_era_probe_shuffle_v1", "raw"));
    let masked = era_probe::train(&tr_x.1, &tr_y, dims, 5, 0.2, 1e-6, rank(&a.probe_seed, "cc_era_probe_shuffle_v1", "masked"));
    let probe = Probe { raw, masked, dims };
    let mut late = (Vec::new(), Vec::new());
    let mut early = (Vec::new(), Vec::new());
    for &(i, class) in &te {
        let (r, m) = probe.logits(pool[i].scored_text().0);
        if class { late.0.push(r); late.1.push(m) } else { early.0.push(r); early.1.push(m) }
    }
    let info = json!({"design": "era_probe v1 selection, split, features and hyperparameters (dims 2^20, 5 epochs, lr 0.2, l2 1e-6), probe seed as recorded; pool documents in any batch source group excluded",
        "probe_seed": a.probe_seed, "excluded_pool_documents": excluded, "train_documents": tr_y.len(), "heldout_documents": te.len(),
        "heldout_auc_late_vs_early_raw": auc(&late.0, &early.0), "heldout_auc_late_vs_early_masked": auc(&late.1, &early.1)});
    Ok((probe, info))
}

fn load_preds(path: &std::path::Path) -> Result<HashMap<String, f64>> {
    let mut m = HashMap::new();
    for v in read_jsonl(path)? {
        let id = v["id"].as_str().context("id")?.to_string();
        let a = v["prediction"]["ai_confidence_percent"].as_f64().with_context(|| format!("ai_confidence_percent for {id}"))? / 100.0;
        m.insert(id, a);
    }
    Ok(m)
}

/// AUC with a cluster bootstrap over pairs (each cluster holds one mirror and its parent).
fn pair_auc(mirror: &[f64], parent: &[f64], draws: usize, seed: u64) -> Value {
    let n = mirror.len();
    let a0 = auc(mirror, parent);
    let mut rng = Rng(seed);
    let mut vals = Vec::with_capacity(draws);
    for _ in 0..draws {
        let idx: Vec<usize> = (0..n).map(|_| rng.below(n)).collect();
        let m: Vec<f64> = idx.iter().map(|&i| mirror[i]).collect();
        let p: Vec<f64> = idx.iter().map(|&i| parent[i]).collect();
        vals.push(auc(&m, &p));
    }
    let wins = (0..n).filter(|&i| mirror[i] > parent[i]).count() as f64 / n.max(1) as f64;
    json!({"pairs": n, "auc": a0, "auc_pair_bootstrap95": [percentile(&mut vals.clone(), 0.025), percentile(&mut vals, 0.975)],
        "share_pairs_mirror_above_parent": wins})
}

fn mean(v: &[f64]) -> f64 {
    v.iter().sum::<f64>() / v.len().max(1) as f64
}

fn var1(v: &[f64]) -> f64 {
    if v.len() < 2 {
        return 0.0;
    }
    let m = mean(v);
    v.iter().map(|x| (x - m) * (x - m)).sum::<f64>() / (v.len() - 1) as f64
}

pub fn gates(a: &GatesArgs) -> Result<Value> {
    ensure!(!a.preds.is_empty(), "the reference prediction file is required");
    ensure!(a.gate_defs == GATE_DEFS_V1 || a.gate_defs == GATE_DEFS_V2, "unknown gate definitions {}", a.gate_defs);
    let docs: Vec<Doc> = store::rows_plain(&a.cohort)?;
    let parents: Vec<Parent> = era_batch::read_parents(&a.batch)?;
    let groups: HashSet<String> = parents.iter().map(|p| p.group.clone()).collect();
    eprintln!("training the era probe ({} batch groups excluded)", groups.len());
    let (probe, probe_info) = train_probe(a, &groups)?;
    eprintln!("scoring documents and parents");
    let doc_logits: Vec<(f64, f64)> = docs.par_iter().map(|d| probe.logits(&d.text)).collect();
    let parent_logits: HashMap<&str, (f64, f64)> = parents.par_iter().map(|p| (p.id.as_str(), probe.logits(&p.text))).collect();
    let parent_text: HashMap<&str, &str> = parents.iter().map(|p| (p.id.as_str(), p.text.as_str())).collect();

    // ---------------------------------------------------------- gate 1
    let mut gate1 = BTreeMap::new();
    for (role, label) in [(MIRROR_DATED, "dated"), (MIRROR_TOPIC, "topic_only"), (MIRROR_PLAIN, "unconditioned")] {
        let idx: Vec<usize> = (0..docs.len()).filter(|&i| docs[i].role == role).collect();
        let mut arm = BTreeMap::new();
        for (vi, vname) in [(0usize, "raw"), (1, "digits_masked")] {
            let pick = |t: (f64, f64)| if vi == 0 { t.0 } else { t.1 };
            let m: Vec<f64> = idx.iter().map(|&i| pick(doc_logits[i])).collect();
            let p: Vec<f64> = idx.iter().map(|&i| pick(parent_logits[docs[i].parent_id.as_str()])).collect();
            let mut v = pair_auc(&m, &p, a.draws, 0xe7a_0001 + vi as u64);
            // Mirrors against the cohort's human-only negatives (what a miner sees).
            let hum: Vec<f64> = (0..docs.len()).filter(|&i| docs[i].role == HUMAN).map(|i| pick(doc_logits[i])).collect();
            v["auc_vs_cohort_human_only"] = json!(auc(&m, &hum));
            let mut by_gen: BTreeMap<String, (Vec<f64>, Vec<f64>)> = BTreeMap::new();
            let mut by_dom: BTreeMap<String, (Vec<f64>, Vec<f64>)> = BTreeMap::new();
            let mut by_era: BTreeMap<String, (Vec<f64>, Vec<f64>)> = BTreeMap::new();
            for (k, &i) in idx.iter().enumerate() {
                let g = by_gen.entry(docs[i].generator.clone().unwrap_or_default()).or_default();
                g.0.push(m[k]);
                g.1.push(p[k]);
                let d = by_dom.entry(docs[i].domain.clone()).or_default();
                d.0.push(m[k]);
                d.1.push(p[k]);
                let y = docs[i].year;
                let e = by_era.entry(if y <= 2016 { "2013-2016" } else if y <= 2019 { "2017-2019" } else { "2020-2022" }.to_string()).or_default();
                e.0.push(m[k]);
                e.1.push(p[k]);
            }
            let f = |b: BTreeMap<String, (Vec<f64>, Vec<f64>)>| -> BTreeMap<String, Value> {
                b.into_iter().map(|(k, (m, p))| (k, json!({"pairs": m.len(), "auc": auc(&m, &p)}))).collect()
            };
            v["by_generator"] = json!(f(by_gen));
            v["by_domain"] = json!(f(by_dom));
            v["by_parent_year"] = json!(f(by_era));
            v["mean_p_late_mirrors"] = json!(mean(&m.iter().map(|&z| sigmoid(z)).collect::<Vec<_>>()));
            v["mean_p_late_parents"] = json!(mean(&p.iter().map(|&z| sigmoid(z)).collect::<Vec<_>>()));
            arm.insert(vname.to_string(), v);
        }
        gate1.insert(label.to_string(), arm);
    }
    // Probe n-gram buckets that push dated mirrors later than their parents.
    let contrib = {
        let idx: Vec<usize> = (0..docs.len()).filter(|&i| docs[i].role == MIRROR_DATED).collect();
        let mut diff: HashMap<u32, f64> = HashMap::new();
        let add = |t: &str, sign: f64, diff: &mut HashMap<u32, f64>| {
            let x = era_probe::features(t, false, probe.dims);
            let sc = if x.is_empty() { 0.0 } else { 1.0 / (x.len() as f64).sqrt() };
            for j in x {
                *diff.entry(j).or_default() += sign * probe.raw.w[j as usize] * sc;
            }
        };
        for &i in &idx {
            add(&docs[i].text, 1.0, &mut diff);
            add(parent_text[docs[i].parent_id.as_str()], -1.0, &mut diff);
        }
        let npairs = idx.len().max(1) as f64;
        let mut v: Vec<(u32, f64)> = diff.into_iter().map(|(k, x)| (k, x / npairs)).collect();
        v.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
        let top: Vec<(u32, f64)> = v.iter().take(30).copied().chain(v.iter().rev().take(15).copied()).collect();
        let want: HashSet<u32> = top.iter().map(|x| x.0).collect();
        let mask = (1u64 << probe.dims) - 1;
        let mut names: HashMap<u32, HashMap<String, (usize, usize)>> = HashMap::new();
        for &i in &idx {
            for (t, side) in [(docs[i].text.as_str(), 0usize), (parent_text[docs[i].parent_id.as_str()], 1usize)] {
                let mut seen = HashSet::new();
                for g in era_probe::ngrams(&era_probe::tokens(t, false)) {
                    let j = (era_probe::fnv1a(g.as_bytes()) & mask) as u32;
                    if want.contains(&j) && seen.insert(g.clone()) {
                        let e = names.entry(j).or_default().entry(g).or_default();
                        if side == 0 { e.0 += 1 } else { e.1 += 1 }
                    }
                }
            }
        }
        let name = |j: u32, d: f64| {
            let mut c: Vec<(String, (usize, usize))> = names.get(&j).map(|m| m.iter().map(|(k, v)| (k.clone(), *v)).collect()).unwrap_or_default();
            c.sort_by(|a, b| (b.1.0 + b.1.1).cmp(&(a.1.0 + a.1.1)).then(a.0.cmp(&b.0)));
            let f = c.first();
            json!({"ngram": f.map(|x| x.0.clone()), "mirror_docs": f.map(|x| x.1.0), "parent_docs": f.map(|x| x.1.1),
                "mean_logit_contribution_mirror_minus_parent": d, "probe_weight": probe.raw.w[j as usize]})
        };
        json!({"pairs": idx.len(), "toward_late": top.iter().take(30).map(|&(j, d)| name(j, d)).collect::<Vec<_>>(),
            "toward_early": top.iter().skip(30).map(|&(j, d)| name(j, d)).collect::<Vec<_>>(),
            "mean_logit_mirror_minus_parent": v.iter().map(|x| x.1).sum::<f64>(),
            "share_of_shift_from_top_30_late_buckets": v.iter().take(30).map(|x| x.1).sum::<f64>() / v.iter().map(|x| x.1).sum::<f64>()})
    };
    let g1_auc = gate1["dated"]["raw"]["auc"].as_f64().unwrap_or(f64::NAN);
    let g1_hi = gate1["dated"]["raw"]["auc_pair_bootstrap95"][1].as_f64().unwrap_or(f64::NAN);

    // ---------------------------------------------------------- cohort and reference
    let members: Vec<usize> = (0..docs.len()).filter(|&i| docs[i].cohort_member).collect();
    let y: Vec<f64> = members.iter().map(|&i| docs[i].y as f64).collect();
    let n = members.len();
    // Target origin mix 20/55/25: weight target share / realised share (1 when the batch is exact).
    let mut realised: BTreeMap<&str, usize> = BTreeMap::new();
    for &i in &members {
        *realised.entry(docs[i].origin.as_str()).or_default() += 1;
    }
    let target = |o: &str| match o { "human_only" => TARGET_MIX[0], "mixed" => TARGET_MIX[1], _ => TARGET_MIX[2] };
    let w: Vec<f64> = members.iter().map(|&i| target(&docs[i].origin) / (realised[docs[i].origin.as_str()] as f64 / n as f64)).collect();
    let mut systems: Vec<(String, Vec<f64>)> = Vec::new();
    let mut sys_files = BTreeMap::new();
    for (name, path) in &a.preds {
        let m = load_preds(path)?;
        let v: Vec<f64> = members.iter().map(|&i| m.get(&docs[i].id).copied().with_context(|| format!("{name}: no prediction for {}", docs[i].id))).collect::<Result<_>>()?;
        sys_files.insert(name.clone(), json!({"path": path, "sha256": file_sha256(path)?, "cohort_rows": v.len()}));
        systems.push((name.clone(), v));
    }
    let all: Vec<usize> = (0..n).collect();
    let r0 = &systems[0].1;
    let b0 = brier(r0, &y, &w, &all);
    let origin_of = |k: usize| docs[members[k]].origin.as_str();
    let mut by_origin = BTreeMap::new();
    for o in ["human_only", "mixed", "model_only"] {
        let idx: Vec<usize> = (0..n).filter(|&k| origin_of(k) == o).collect();
        let loss: f64 = idx.iter().map(|&k| w[k] * (r0[k] - y[k]).powi(2)).sum::<f64>();
        let flags = idx.iter().filter(|&&k| r0[k] >= a.threshold).count();
        let wil = if idx.is_empty() { Value::Null } else { json!(measure::wilson(flags, idx.len())) };
        by_origin.insert(o.to_string(), json!({"n": idx.len(), "mean_brier": brier(r0, &y, &w, &idx), "loss_share": loss / (b0 * w.iter().sum::<f64>()), "weight": w.get(idx.first().copied().unwrap_or(0)).copied().unwrap_or(0.0),
            "flag_rate_at_cal_1pct": flags as f64 / idx.len().max(1) as f64, "flagged": flags, "wilson95": wil}));
    }
    let mut by_domain_origin = BTreeMap::new();
    let mut cells: BTreeMap<(String, String), Vec<usize>> = BTreeMap::new();
    for k in 0..n {
        cells.entry((docs[members[k]].domain.clone(), origin_of(k).to_string())).or_default().push(k);
    }
    for ((d, o), idx) in &cells {
        by_domain_origin.insert(format!("{d}|{o}"), json!({"n": idx.len(), "mean_brier_r0": brier(r0, &y, &w, idx)}));
    }
    let mut by_gen = BTreeMap::new();
    let mut gcells: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    for k in 0..n {
        if let Some(g) = &docs[members[k]].generator {
            gcells.entry(format!("{}|{g}", origin_of(k))).or_default().push(k);
        }
    }
    for (g, idx) in &gcells {
        by_gen.insert(g.clone(), json!({"n": idx.len(), "mean_brier_r0": brier(r0, &y, &w, idx)}));
    }

    // ---------------------------------------------------------- sigma, delta, w
    let mut seed_rows = BTreeMap::new();
    let mut se_by_n: BTreeMap<usize, Vec<f64>> = BTreeMap::new();
    let mut boot_se = Vec::new();
    let mut dq_boot_all: Vec<Vec<f64>> = Vec::new();
    // Stratified bootstrap replicate indices (shared across systems for pairing).
    let cell_list: Vec<&Vec<usize>> = cells.values().collect();
    let mut rng = Rng(0x5eed_e7a2);
    let mut reps: Vec<Vec<usize>> = Vec::with_capacity(a.draws);
    for _ in 0..a.draws {
        let mut ix = Vec::with_capacity(n);
        for c in &cell_list {
            for _ in 0..c.len() {
                ix.push(c[rng.below(c.len())]);
            }
        }
        reps.push(ix);
    }
    for (name, s) in systems.iter().skip(1) {
        let bs = brier(s, &y, &w, &all);
        let r = bs / b0;
        let dq_point = (b0 - bs) / b0;
        let mut row = json!({"mean_brier": bs, "dq_point_vs_r0": dq_point});
        for &big_n in &[1000usize, 2000, 4000] {
            let mut v = 0.0;
            for idx in cells.values() {
                let w_c = idx.iter().map(|&k| w[k]).sum::<f64>() / w.iter().sum::<f64>();
                let d: Vec<f64> = idx.iter().map(|&k| ((s[k] - y[k]).powi(2) - r * (r0[k] - y[k]).powi(2)) / b0).collect();
                v += w_c * var1(&d) / big_n as f64;
            }
            let se = v.sqrt();
            row[format!("se_dq_analytic_n{big_n}")] = json!(se);
            se_by_n.entry(big_n).or_default().push(se);
        }
        let dq: Vec<f64> = reps.iter().map(|ix| {
            let br = brier(r0, &y, &w, ix);
            (br - brier(s, &y, &w, ix)) / br
        }).collect();
        let sd = var1(&dq).sqrt();
        row[format!("se_dq_bootstrap_n{n}")] = json!(sd);
        boot_se.push(sd);
        dq_boot_all.push(dq);
        seed_rows.insert(name.clone(), row);
    }
    let pooled = |v: &[f64]| (v.iter().map(|x| x * x).sum::<f64>() / v.len() as f64).sqrt();
    let mut sigma = BTreeMap::new();
    for (big_n, ses) in &se_by_n {
        let s = pooled(ses);
        let delta = (2.0 * s).max(0.01);
        sigma.insert(format!("n{big_n}"), json!({"sigma": s, "delta": delta, "w_share_of_headroom": 2.0 * s, "w_brier": 2.0 * s * b0,
            "epsilon_brier": 2.0 * s * b0 / 250.0}));
    }
    let rho = if dq_boot_all.len() >= 2 {
        let (x, z) = (&dq_boot_all[0], &dq_boot_all[1]);
        let (mx, mz) = (mean(x), mean(z));
        let sxz: f64 = x.iter().zip(z).map(|(a, b)| (a - mx) * (b - mz)).sum();
        let sxx: f64 = x.iter().map(|a| (a - mx) * (a - mx)).sum();
        let szz: f64 = z.iter().map(|b| (b - mz) * (b - mz)).sum();
        json!(sxz / (sxx * szz).sqrt())
    } else {
        json!(null)
    };

    // ---------------------------------------------------------- adversaries
    eprintln!("adversaries on {n} cohort documents");
    let (feat_sets, fmt_summary): (Vec<FeatureSet>, Value) = {
        let era: Vec<(Vec<String>, Vec<f64>)> = members.iter().map(|&i| era_features(&docs[i].text, doc_logits[i].0, doc_logits[i].1)).collect();
        let fmt: Vec<(Vec<String>, Vec<f64>)> = members.iter().map(|&i| format_features(&docs[i].text)).collect();
        let len: Vec<(Vec<String>, Vec<f64>)> = members.iter().map(|&i| length_features(&docs[i].text)).collect();
        let lw: Vec<(Vec<String>, Vec<f64>)> = members.iter().map(|&i| word_count_features(&docs[i].text)).collect();
        let lc: Vec<(Vec<String>, Vec<f64>)> = members.iter().map(|&i| mean_word_length_features(&docs[i].text)).collect();
        let fmt_rows: Vec<Vec<f64>> = fmt.iter().map(|x| x.1.clone()).collect();
        let member_origins: Vec<&str> = members.iter().map(|&i| docs[i].origin.as_str()).collect();
        let summary = format_summary(&fmt[0].0, &fmt_rows, &member_origins);
        // Diagnostic split of the format set: without the character-class block, and that block alone.
        let is_cc: Vec<bool> = fmt[0].0.iter().map(|k| CHAR_CLASS_FEATURES.contains(&k.as_str())).collect();
        let pick = |keep: bool| -> (Vec<String>, Vec<Vec<f64>>) {
            let names: Vec<String> = fmt[0].0.iter().zip(&is_cc).filter(|(_, c)| **c != keep).map(|(k, _)| k.clone()).collect();
            let rows: Vec<Vec<f64>> = fmt_rows.iter().map(|r| r.iter().zip(&is_cc).filter(|(_, c)| **c != keep).map(|(x, _)| *x).collect()).collect();
            (names, rows)
        };
        let (no_cc_names, no_cc) = pick(true);
        let (cc_names, cc) = pick(false);
        let both: Vec<Vec<f64>> = era.iter().zip(&fmt).map(|(e, f)| e.1.iter().chain(&f.1).copied().collect()).collect();
        let both_names: Vec<String> = era[0].0.iter().chain(&fmt[0].0).cloned().collect();
        // Explicit era cues alone: year mentions and the recency lexicon, without the probe logits.
        let explicit: Vec<Vec<f64>> = era.iter().map(|e| e.1[2..].to_vec()).collect();
        let explicit_names: Vec<String> = era[0].0[2..].to_vec();
        let sets = vec![
            ("era_explicit_only_diagnostic", explicit_names, explicit),
            ("era_only", era[0].0.clone(), era.into_iter().map(|x| x.1).collect()),
            ("format_only", fmt[0].0.clone(), fmt_rows),
            ("format_layout_typography", no_cc_names, no_cc),
            ("format_char_class", cc_names, cc),
            ("era_and_format", both_names, both),
            ("length_only_diagnostic", len[0].0.clone(), len.into_iter().map(|x| x.1).collect()),
            ("length_word_count_diagnostic", lw[0].0.clone(), lw.into_iter().map(|x| x.1).collect()),
            ("length_mean_word_length_diagnostic", lc[0].0.clone(), lc.into_iter().map(|x| x.1).collect()),
        ];
        (sets, summary)
    };
    let r0_logit: Vec<f64> = r0.iter().map(|&p| logit(p)).collect();
    let mut adversaries = BTreeMap::new();
    let mut gains_for_gate = BTreeMap::new();
    for (name, fnames, feats) in &feat_sets {
        let mut repeats = Vec::new();
        let mut first: Option<(Preds, Vec<f64>, Preds, Preds)> = None;
        for rep in 0..a.cv_repeats {
            let folds: Vec<usize> = members.iter().map(|&i| (rank(&a.seed, &format!("cc_era_fold_v1:{rep}"), &docs[i].group) % a.folds as u64) as usize).collect();
            let (preds, prior) = cross_fit(feats, &y, &w, &folds, a.folds)?;
            let bp = brier(&prior, &y, &w, &all);
            let gains: BTreeMap<String, f64> = preds.iter().map(|(k, p)| (k.clone(), (bp - brier(p, &y, &w, &all)) / b0)).collect();
            // Incremental: the reference logit alone against it plus the features.
            let base_feats: Vec<Vec<f64>> = r0_logit.iter().map(|&z| vec![z]).collect();
            let inc_feats: Vec<Vec<f64>> = feats.iter().zip(&r0_logit).map(|(f, &z)| std::iter::once(z).chain(f.iter().copied()).collect()).collect();
            let (base_p, _) = cross_fit(&base_feats, &y, &w, &folds, a.folds)?;
            let (inc_p, _) = cross_fit(&inc_feats, &y, &w, &folds, a.folds)?;
            let base_best = base_p.values().map(|p| brier(p, &y, &w, &all)).fold(f64::INFINITY, f64::min);
            let inc: BTreeMap<String, f64> = inc_p.iter().map(|(k, p)| (k.clone(), (base_best - brier(p, &y, &w, &all)) / b0)).collect();
            let best = gains.values().copied().fold(f64::NEG_INFINITY, f64::max);
            let best_inc = inc.values().copied().fold(f64::NEG_INFINITY, f64::max);
            repeats.push(json!({"repeat": rep, "prior_brier": bp, "gain_share_of_b0": gains, "best_gain_share_of_b0": best,
                "r0_recalibrated_brier": base_best, "incremental_gain_share_of_b0": inc, "best_incremental_gain_share_of_b0": best_inc}));
            if first.is_none() {
                first = Some((preds, prior, inc_p, base_p));
            }
        }
        let bests: Vec<f64> = repeats.iter().map(|r| r["best_gain_share_of_b0"].as_f64().unwrap()).collect();
        let bests_inc: Vec<f64> = repeats.iter().map(|r| r["best_incremental_gain_share_of_b0"].as_f64().unwrap()).collect();
        // Bootstrap (documents, stratified by cell) of the best configuration of repeat 0.
        let (preds, prior, inc_p, base_p) = first.unwrap();
        let (bk, bv) = preds.iter().max_by(|x, z| {
            let gx = brier(&prior, &y, &w, &all) - brier(x.1, &y, &w, &all);
            let gz = brier(&prior, &y, &w, &all) - brier(z.1, &y, &w, &all);
            gx.total_cmp(&gz)
        }).unwrap();
        let mut boot: Vec<f64> = reps.iter().map(|ix| (brier(&prior, &y, &w, ix) - brier(bv, &y, &w, ix)) / brier(r0, &y, &w, ix)).collect();
        let lo = percentile(&mut boot, 0.025);
        let hi = percentile(&mut boot, 0.975);
        let (ik, iv) = inc_p.iter().min_by(|x, z| brier(x.1, &y, &w, &all).total_cmp(&brier(z.1, &y, &w, &all))).unwrap();
        let (_, bb) = base_p.iter().min_by(|x, z| brier(x.1, &y, &w, &all).total_cmp(&brier(z.1, &y, &w, &all))).unwrap();
        let mut iboot: Vec<f64> = reps.iter().map(|ix| (brier(bb, &y, &w, ix) - brier(iv, &y, &w, ix)) / brier(r0, &y, &w, ix)).collect();
        // Per-origin mean prediction of the best configuration (where the signal sits).
        let mut po = BTreeMap::new();
        for o in ["human_only", "mixed", "model_only"] {
            let idx: Vec<usize> = (0..n).filter(|&k| origin_of(k) == o).collect();
            po.insert(o.to_string(), json!({"mean_prediction": mean(&idx.iter().map(|&k| bv[k]).collect::<Vec<_>>()), "mean_prior": mean(&idx.iter().map(|&k| prior[k]).collect::<Vec<_>>())}));
        }
        // Linear standardised coefficients on the full cohort (descriptive only).
        let refs: Vec<&Vec<f64>> = feats.iter().collect();
        let d = Design::fit(&refs, false);
        let x: Vec<Vec<f64>> = feats.iter().map(|f| d.row(f)).collect();
        let beta = fit_logistic(&x, &y, &w, 1.0)?;
        let mut coefs: Vec<(String, f64)> = fnames.iter().cloned().zip(beta[1..].iter().copied()).collect();
        coefs.sort_by(|p, q| q.1.abs().total_cmp(&p.1.abs()));
        let point = mean(&bests);
        adversaries.insert(name.to_string(), json!({
            "features": fnames, "feature_count": fnames.len(),
            "best_gain_share_of_b0_mean_over_repeats": point, "best_gain_share_of_b0_min": bests.iter().copied().fold(f64::INFINITY, f64::min),
            "best_gain_share_of_b0_max": bests.iter().copied().fold(f64::NEG_INFINITY, f64::max),
            "repeat0_best_config": bk, "repeat0_best_gain_bootstrap95": [lo, hi],
            "incremental_best_gain_share_of_b0_mean_over_repeats": mean(&bests_inc), "repeat0_incremental_best_config": ik,
            "repeat0_incremental_bootstrap95": [percentile(&mut iboot.clone(), 0.025), percentile(&mut iboot, 0.975)],
            "by_origin_repeat0_best": po, "top_linear_coefficients_full_cohort": coefs.iter().take(12).map(|(k, v)| json!({"feature": k, "beta_per_sd": v})).collect::<Vec<_>>(),
            "repeats": repeats,
        }));
        gains_for_gate.insert(name.to_string(), (point, hi));
    }
    let gain = |k: &str| json!({"gain_share_of_b0": gains_for_gate[k].0, "upper95": gains_for_gate[k].1});
    let gate = |k: &str| {
        let (g, hi) = gains_for_gate[k];
        json!({"adversary": k, "gain_share_of_b0": g, "upper95": hi, "gate": ADVERSARY_GATE, "pass": g <= ADVERSARY_GATE, "upper_bound_also_passes": hi <= ADVERSARY_GATE})
    };
    let probe = json!({"auc": g1_auc, "upper95": g1_hi, "reference_value": AUC_GATE, "at_or_below_reference": g1_auc <= AUC_GATE});
    let length = json!({"length_only": gain("length_only_diagnostic"), "word_count_only": gain("length_word_count_diagnostic"),
        "mean_word_length_only": gain("length_mean_word_length_diagnostic")});
    let mut probe_gate = probe.clone();
    probe_gate["gate"] = json!(AUC_GATE);
    probe_gate["pass"] = json!(g1_auc <= AUC_GATE);
    probe_gate["upper_bound_also_passes"] = json!(g1_hi <= AUC_GATE);
    let v1 = json!({
        "definitions": GATE_DEFS_V1,
        "gates": {"era_probe_auc_parents_vs_dated_mirrors": probe_gate, "era_only_adversary": gate("era_only"), "format_only_adversary": gate("format_only")},
        "diagnostics": {"format_char_class_split": {"char_class_features": CHAR_CLASS_FEATURES, "without_char_class": gain("format_layout_typography"),
            "char_class_only": gain("format_char_class")}, "length": length.clone()},
    });
    let v2 = json!({
        "definitions": GATE_DEFS_V2,
        "gates": {"era_only_adversary": gate("era_only"), "format_layout_typography_adversary": gate("format_layout_typography")},
        "diagnostics": {"era_probe_auc_parents_vs_dated_mirrors": probe, "format_char_class_adversary": gain("format_char_class"),
            "format_all_features_adversary": gain("format_only"), "char_class_features": CHAR_CLASS_FEATURES, "length": length},
    });
    let pass_all = |v: &Value| v["gates"].as_object().is_some_and(|m| m.values().all(|g| g["pass"] == true));
    let mut verdict = if a.gate_defs == GATE_DEFS_V1 { v1.clone() } else { v2.clone() };
    verdict["all_gates_pass"] = json!(pass_all(&verdict));
    let by_definition = json!({GATE_DEFS_V1: {"all_gates_pass": pass_all(&v1), "verdict": v1}, GATE_DEFS_V2: {"all_gates_pass": pass_all(&v2), "verdict": v2}});
    let mut origins = BTreeMap::new();
    for &i in &members {
        *origins.entry(docs[i].origin.clone()).or_insert(0usize) += 1;
    }
    let out = json!({
        "schema": GATES_SCHEMA, "verdict": verdict, "verdict_by_definition": by_definition,
        "inputs": {"cohort": a.cohort, "cohort_sha256": file_sha256(&a.cohort)?, "batch_sha256": file_sha256(&a.batch.join("BATCH.json"))?,
            "pool_sha256": file_sha256(&a.pool)?, "systems": sys_files, "reference": systems[0].0, "threshold": a.threshold,
            "seed": a.seed, "folds": a.folds, "cv_repeats": a.cv_repeats, "draws": a.draws, "lambdas": LAMBDAS},
        "probe": probe_info,
        "gate1_era_probe": gate1, "gate1_probe_shift_buckets": contrib,
        "cohort": {"documents": n, "by_origin": origins, "target_mix": TARGET_MIX,
            "mix_note": "self-weighting within origin; each origin reweighted to the 20/55/25 target (weight = target share / realised share), so every Brier mean, prior, fit and SE is on the launch estimand"},
        "reference": {"name": systems[0].0, "b0_mean_brier": b0, "by_origin": by_origin, "by_domain_origin": by_domain_origin, "by_origin_generator": by_gen},
        "seeds": seed_rows, "sigma": sigma, "seed_pair_dq_correlation_bootstrap": rho,
        "sigma_method": "analytic linearised SE of dq = (B_R0 - B_s)/B_R0, stratified by domain x origin at the batch's realised cell shares, scaled to n; pooled over the retrain seeds as sqrt(mean(se^2)); bootstrap SE at the realised n reported per seed; delta = max(0.01, 2 sigma); w = 2 sigma B0",
        "adversaries": adversaries,
        "format_summary_by_origin": fmt_summary,
        "feature_ids": {"era": ERA_FEATURES_ID, "format": FORMAT_FEATURES_ID, "recency_lexicon": RECENCY_LEXICON.iter().map(|(k, v)| json!({"category": k, "terms": v})).collect::<Vec<_>>(), "nav_terms": NAV_TERMS},
    });
    store::write_json(&a.out.join("gates.json"), &out)?;
    Ok(json!({"verdict": out["verdict"], "b0": b0, "sigma": out["sigma"]}))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn era_features_count_years_and_lexicon() {
        let (n, v) = era_features("In 2021 the pandemic moved meetings to Zoom. By 2024 apps had changed.", 0.5, 0.25);
        let get = |k: &str| v[n.iter().position(|x| x == k).unwrap()];
        assert_eq!(get("probe_logit_raw"), 0.5);
        assert_eq!(get("any_year"), 1.0);
        assert_eq!(get("share_years_ge_2023"), 0.5);
        assert_eq!(get("any_year_ge_2023"), 1.0);
        assert_eq!(get("lexicon_pandemic_any"), 1.0);
        assert!(get("lexicon_pandemic_per_100_words") > 0.0);
        assert_eq!(get("lexicon_crypto_any"), 0.0);
        let (_, v2) = era_features("No dates here at all.", 0.0, 0.0);
        assert_eq!(v2.len(), v.len());
    }

    #[test]
    fn format_features_see_quotes_dashes_and_layout() {
        let (n, v) = format_features("“Hello,” she said — twice.\n\n- item one\n- item two\nHome | Share");
        let get = |k: &str| v[n.iter().position(|x| x == k).unwrap()];
        assert!(get("curly_double_quote_per_1k") > 0.0);
        assert_eq!(get("share_curly_among_quotes"), 1.0);
        assert!(get("em_dash_per_1k") > 0.0);
        assert_eq!(get("bullet_lines"), 2.0);
        assert_eq!(get("paragraphs_blank_separated"), 2.0);
        assert!(get("separator_lines") >= 1.0);
        assert!(get("nav_terms_per_100_words") > 0.0);
        let (n2, _) = format_features("plain");
        assert_eq!(n2, n);
    }

    #[test]
    fn length_parts_split_words_and_mean_word_length() {
        let t = "Seven short words here in this line.";
        let (_, all) = length_features(t);
        let (_, w) = word_count_features(t);
        let (_, c) = mean_word_length_features(t);
        assert_eq!(w[0], all[0]);
        assert!((c[0] - (all[1] - all[0])).abs() < 1e-12);
        // The layout and typography set is the format set without the character-class block.
        let (names, _) = format_features(t);
        assert_eq!(names.iter().filter(|n| CHAR_CLASS_FEATURES.contains(&n.as_str())).count(), CHAR_CLASS_FEATURES.len());
        assert_eq!((names.len(), names.len() - CHAR_CLASS_FEATURES.len()), (51, 43));
        assert_eq!(DEFAULT_GATE_DEFS, GATE_DEFS_V2);
    }

    #[test]
    fn normaliser_v0_maps_typography_and_layout() {
        assert_eq!(normalise_v0("\u{201C}Hi,\u{201D} she said\u{2014}twice\u{2026}\r\n  \n\tNext\u{00A0} line  here "), "\"Hi,\" she said-twice...\n\nNext line here");
        assert_eq!(normalise_v0("a\n\n\nb\nc"), "a\n\nb\n\nc");
        assert_eq!(normalise_v0("it\u{2019}s"), "it's");
        assert_eq!(flatten_v0("a  b\n\nc\nd "), "a b c d");
    }

    #[test]
    fn logistic_recovers_a_signal_and_cross_fit_beats_the_prior() {
        let mut rng = Rng(11);
        let mut feats = Vec::new();
        let mut y = Vec::new();
        for _ in 0..600 {
            let label = rng.below(5) != 0;
            let x = if label { 1.0 } else { 0.0 } + (rng.below(1000) as f64 / 1000.0 - 0.5);
            feats.push(vec![x, rng.below(1000) as f64 / 1000.0]);
            y.push(label as u8 as f64);
        }
        let folds: Vec<usize> = (0..feats.len()).map(|i| i % 5).collect();
        let w = vec![1.0; feats.len()];
        let (preds, prior) = cross_fit(&feats, &y, &w, &folds, 5).unwrap();
        let all: Vec<usize> = (0..feats.len()).collect();
        let bp = brier(&prior, &y, &w, &all);
        for (k, p) in &preds {
            assert!(brier(p, &y, &w, &all) < bp, "{k}");
        }
        let mut a = vec![4.0, 2.0, 2.0, 3.0];
        let mut b = vec![2.0, 1.0];
        solve_spd(&mut a, &mut b, 2).unwrap();
        assert!((b[0] - 0.5).abs() < 1e-12 && b[1].abs() < 1e-12);
    }
}
