//! Pass 2: site templates, keyed paragraph-run choice and excerpt-level
//! English, producing one candidate per page.
//!
//! * **Site templates.** A line of 8+ words whose whitespace-normalised hash
//!   occurs on two or more surviving pilot pages of one registered domain is
//!   a break (the FineWeb adapter's rule, counted over the whole pilot).
//! * **Lines.** Each WET line is a paragraph, classified by the FineWeb
//!   adapter's `classify` (list markers, boilerplate terms and `@`, site
//!   templates, then the shared prose screen with markup and quotation
//!   breaks).
//! * **Passage choice.** Every valid run (consecutive prose lines, stopping
//!   at the first line boundary at or after 220 words, 150..=450 words, a
//!   shorter run only when it ends at a break) is a candidate; the run with
//!   the lowest `rank(seed, "cc_human_passage_v1", record_id:start)` is
//!   kept, and the number of valid runs is recorded.
//! * **English** on the chosen excerpt (`lang`). E-mail addresses and phone
//!   numbers are redacted in `text_redacted`; `text` stays exact.
//! * **Final domain.** The rules run again with the excerpt (rules v2 gate
//!   content cues and tier-2 rules on it); the pass-1 page domain is kept
//!   as `page_domain`. The excerpt screens (explicit text) run last.
use crate::{
    domain::{self, Rules},
    funnel::Funnel,
    lang::English,
    pages::{self, Page},
    screens, store,
};
use anyhow::Result;
use grammar_a_pools::{
    fineweb,
    splits::rank,
    text::{self, Para},
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{
    collections::{BTreeMap, HashSet},
    path::{Path, PathBuf},
    time::Instant,
};

pub const CANDIDATE_SCHEMA: &str = "slop_ninja_cc_human_candidate_v3";
pub const PASSAGE_SCOPE: &str = "cc_human_passage_v1";
pub const EXCERPT_METHOD: &str =
    "wet_lines_as_paragraphs;fineweb_classify_with_pilot_site_templates;all_valid_runs_150_450_stop_at_220;keyed_rank_choice_v1";
pub const RIGHTS_BASIS: &str = "commoncrawl_tou_2024-03-07;third_party_content;private_evaluation;project_policy_2026-10-27";
pub const RIGHTS_CLASS: &str = "restricted_no_redistribution";

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Candidate {
    pub schema: String,
    pub id: String,
    pub crawl: String,
    pub wet_path: String,
    pub wet_sha256: String,
    pub member_offset: u64,
    pub member_length: u64,
    pub record_id: String,
    pub refers_to: Option<String>,
    pub warc_date: String,
    pub url: String,
    pub host: String,
    pub registered_domain: String,
    pub block_digest: Option<String>,
    pub cld2: Option<String>,
    pub page_sha256: String,
    pub page_words: usize,
    pub domain: String,
    pub domain_rule: String,
    pub cues: BTreeMap<String, usize>,
    /// Pass-1 (page-only) domain and rule, when the final assignment
    /// read the excerpt (candidate schema v3).
    #[serde(default)]
    pub page_domain: Option<String>,
    #[serde(default)]
    pub page_domain_rule: Option<String>,
    pub group: String,
    pub group_rule: String,
    pub era: String,
    /// Half-open span over the page's non-empty lines.
    pub line_start: usize,
    pub line_end: usize,
    pub valid_runs: usize,
    pub run_rank: String,
    pub words: usize,
    pub chars: usize,
    pub text: String,
    pub text_sha256: String,
    pub normalized_sha256: String,
    pub folded_sha256: String,
    pub text_redacted: Option<String>,
    pub redacted_emails: usize,
    pub redacted_phones: usize,
    pub lang: serde_json::Value,
    pub rights_basis: String,
    pub rights_class: String,
}

/// Every valid paragraph run (start, end, words).
pub fn valid_runs(paras: &[Para]) -> Vec<(usize, usize, usize)> {
    let mut out = Vec::new();
    for i in 0..paras.len() {
        if !paras[i].prose {
            continue;
        }
        let (mut n, mut j) = (0, i);
        while j < paras.len() && paras[j].prose {
            n += text::words(&paras[j].text);
            j += 1;
            if n >= text::TARGET_WORDS {
                break;
            }
        }
        let run_ended = j == paras.len() || !paras[j].prose;
        if (text::MIN_WORDS..=text::MAX_WORDS).contains(&n) && (n >= text::TARGET_WORDS || run_ended) {
            out.push((i, j, n));
        }
    }
    out
}

/// Non-empty trimmed WET lines (the FineWeb adapter's line split).
pub fn lines(t: &str) -> impl Iterator<Item = &str> {
    t.split('\n').map(str::trim).filter(|l| !l.is_empty())
}

pub fn site_line_key(domain: &str, line: &str) -> Option<u64> {
    let n = text::normalized(line);
    (text::words(&n) >= 8).then(|| text::h64(format!("{domain}\0{n}").as_bytes()))
}

/// Keys (registered domain, line) present on each page, one per page.
pub fn page_line_keys(p: &Page) -> Vec<u64> {
    let mut v: Vec<u64> = lines(&p.text).filter_map(|l| site_line_key(&p.registered_domain, l)).collect();
    v.sort_unstable();
    v.dedup();
    v
}

pub fn candidates_path(root: &Path, st: &str) -> PathBuf {
    root.join("candidates").join(format!("{st}.jsonl.zst"))
}

pub fn funnel_path(root: &Path, st: &str) -> PathBuf {
    root.join("funnel").join(format!("{st}.candidates.json"))
}

pub fn process(root: &Path, st: &str, templates: &HashSet<u64>, passage_seed: &str, rules: &Rules) -> Result<bool> {
    let fpath = funnel_path(root, st);
    if fpath.exists() {
        return Ok(false);
    }
    let t0 = Instant::now();
    let pages: Vec<Page> = store::rows(&pages::pages_path(root, st))?;
    let mut out = store::ZWriter::create(&candidates_path(root, st))?;
    let mut rej = store::ZWriter::create(&root.join("rejects").join(format!("{st}.candidates.jsonl.zst")))?;
    let mut f = Funnel::default();
    let mut breaks: BTreeMap<&'static str, u64> = BTreeMap::new();
    let mut lang_patterns: BTreeMap<String, u64> = BTreeMap::new();
    let mut runs_hist: BTreeMap<usize, u64> = BTreeMap::new();
    for p in &pages {
        let paras: Vec<Para> = lines(&p.text)
            .map(|l| {
                let rep = site_line_key(&p.registered_domain, l).is_some_and(|k| templates.contains(&k));
                fineweb::classify(l, rep)
            })
            .collect();
        for x in &paras {
            if let Some(r) = x.reason {
                *breaks.entry(r).or_default() += 1;
            }
        }
        let runs = valid_runs(&paras);
        *runs_hist.entry(runs.len().min(50)).or_default() += 1;
        let reject = |f: &mut Funnel, rej: &mut store::ZWriter, stage: &str, reason: &str| -> Result<()> {
            f.reject(stage, reason, &p.domain);
            rej.write(
                &json!({"record_id": p.record_id, "url": p.url, "domain": p.domain, "stage": stage, "reason": reason, "valid_runs": runs.len()}),
            )
        };
        let Some(&(s, e, _)) = runs
            .iter()
            .min_by_key(|(s, _, _)| rank(passage_seed, PASSAGE_SCOPE, &format!("{}:{s}", p.record_id)))
        else {
            reject(&mut f, &mut rej, "excerpt", "no_paragraph_run_within_bounds")?;
            continue;
        };
        let ex = paras[s..e].iter().map(|x| x.text.as_str()).collect::<Vec<_>>().join("\n\n");
        let ew = text::words(&ex);
        f.pass("excerpt", &p.domain, p.page_words, ew);
        let en = English::check(&ex, p.cld2.as_deref());
        *lang_patterns.entry(en.pattern()).or_default() += 1;
        if let Some(r) = en.reason() {
            reject(&mut f, &mut rej, "english", r)?;
            continue;
        }
        f.pass("english", &p.domain, p.page_words, ew);
        let a = rules.assign_with_excerpt(&p.host, domain::url_path(&p.url), &p.text, &ex);
        if let Some(r) = screens::excerpt_exclusion(&ex) {
            f.reject("excerpt_screens", r, &a.domain);
            rej.write(
                &json!({"record_id": p.record_id, "url": p.url, "domain": a.domain, "page_domain": p.domain,
                              "stage": "excerpt_screens", "reason": r, "valid_runs": runs.len()}),
            )?;
            continue;
        }
        f.pass("excerpt_screens", &a.domain, p.page_words, ew);
        let (red, ne, np) = screens::redact(&ex);
        let uuid = p.record_id.trim_start_matches("<urn:uuid:").trim_end_matches('>');
        out.write(&Candidate {
            schema: CANDIDATE_SCHEMA.to_owned(),
            id: format!("cc-{uuid}"),
            crawl: p.crawl.clone(),
            wet_path: p.wet_path.clone(),
            wet_sha256: p.wet_sha256.clone(),
            member_offset: p.member_offset,
            member_length: p.member_length,
            record_id: p.record_id.clone(),
            refers_to: p.refers_to.clone(),
            warc_date: p.warc_date.clone(),
            url: p.url.clone(),
            host: p.host.clone(),
            registered_domain: p.registered_domain.clone(),
            block_digest: p.block_digest.clone(),
            cld2: p.cld2.clone(),
            page_sha256: p.page_sha256.clone(),
            page_words: p.page_words,
            domain: a.domain,
            domain_rule: a.rule,
            cues: a.cues,
            page_domain: Some(p.domain.clone()),
            page_domain_rule: Some(p.domain_rule.clone()),
            group: p.group.clone(),
            group_rule: p.group_rule.clone(),
            era: p.era.clone(),
            line_start: s,
            line_end: e,
            valid_runs: runs.len(),
            run_rank: format!("{:016x}", rank(passage_seed, PASSAGE_SCOPE, &format!("{}:{s}", p.record_id))),
            words: ew,
            chars: ex.chars().count(),
            text_sha256: text::sha_hex(ex.as_bytes()),
            normalized_sha256: text::normalized_sha(&ex),
            folded_sha256: text::folded_sha(&ex),
            text_redacted: (red != ex).then_some(red),
            redacted_emails: ne,
            redacted_phones: np,
            text: ex,
            lang: en.json(),
            rights_basis: RIGHTS_BASIS.to_owned(),
            rights_class: RIGHTS_CLASS.to_owned(),
        })?;
    }
    let (sha, rows) = out.finish()?;
    rej.finish()?;
    store::write_json(
        &fpath,
        &json!({"schema": "slop_ninja_cc_human_candidates_funnel_v1", "stem": st, "pages": pages.len(), "candidates": rows,
                "candidates_sha256": sha, "paragraph_breaks": breaks, "language_patterns": lang_patterns,
                "valid_runs_histogram_capped_50": runs_hist, "excerpt_method": EXCERPT_METHOD,
                "lang_method": crate::lang::LANG_VERSION, "domain_rules": rules.version, "domain_rules_sha256": rules.sha256,
                "excerpt_screens": screens::EXCERPT_SCREENS_VERSION, "seconds": t0.elapsed().as_secs_f64(), "funnel": f}),
    )?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn p(words: usize, prose: bool) -> Para {
        let t = format!("{}end.", "word ".repeat(words.saturating_sub(1)));
        if prose {
            Para {
                text: t,
                prose: true,
                reason: None,
            }
        } else {
            Para::non_prose(t, "x")
        }
    }
    #[test]
    fn runs_enumerate_every_valid_start() {
        let paras = vec![p(100, true), p(100, true), p(100, true), p(5, false), p(160, true)];
        let r = valid_runs(&paras);
        assert_eq!(r, vec![(0, 3, 300), (1, 3, 200), (4, 5, 160)]);
        assert!(valid_runs(&[p(100, true)]).is_empty());
        assert!(valid_runs(&[p(500, true)]).is_empty());
    }
}
