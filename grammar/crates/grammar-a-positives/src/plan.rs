//! Mechanical positive plan: generator slots, bins, spans, decoding and
//! prompts for every human document of every split, as fixed by
//! `protocol-v1.json` `positive_plan` (evaluation splits) and its Train rule
//! ("same templates and bins; decoding varies within declared ranges;
//! generator mix is a fitting choice").
use crate::text::{self, Idf, Sentence};
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap};

pub const PLAN_KEY_PREFIX: &str = "slopninja-a-comp-plan-v1:";
pub const PLAN_SCHEMA: &str = "slop_ninja_a_comp_positive_plan_v1";
pub const MIRROR_TEMPLATE_ID: &str = "a_comp_mirror_v1";
pub const REWRITE_TEMPLATE_ID: &str = "a_comp_rewrite_v1";
pub const BASE_CONT_TEMPLATE_ID: &str = "a_comp_base_continuation_v1";
pub const BASE_SPLICE_TEMPLATE_ID: &str = "a_comp_base_splice_v1";

pub const REWRITE_INSTRUCTION: &str = "Rewrite the following English passage in your own words while preserving its meaning, facts, perspective, register, and approximate length. Return only the replacement prose, retaining paragraph breaks where useful: no title, preface, analysis, quotation delimiters, or explanation.";

/// Train/Dev instruct families in protocol order.
pub const FAMILY_ORDER: [&str; 7] =
    ["qwen36", "phi4mini", "mistral7b", "gemma3_4b", "granite_instruct", "smollm3_instruct", "ernie_instruct"];
pub const TRAIN_INSTRUCT: [&str; 8] = [
    "qwen36", "phi4mini", "mistral7b", "gemma3_4b", "granite_instruct", "smollm3_instruct", "ernie_instruct", "gptoss20b",
];
pub const TRAIN_BASE: [&str; 3] = ["granite_base", "smollm3_base", "ernie_base"];
pub const CAL_ONLY: &str = "glm4";
/// Test-only generators (protocol `generators.test_only`).
pub const TEST_ONLY_INSTRUCT: &str = "olmo2_instruct";
pub const TEST_ONLY_BASE: &str = "olmo2_base";
pub const TEST_ROBUSTNESS_COHORT: &str = "test_robustness";

pub const SPLITS: [&str; 4] = ["train", "dev_iter", "dev_select", "cal"];
/// Every split with a positive plan, including Test (planned only after the seal).
pub const ALL_SPLITS: [&str; 5] = ["train", "dev_iter", "dev_select", "cal", "test"];

/// The 18 protocol evaluation domains (protocol-v1.json `positive_plan.templates.registers`).
pub const EVAL_DOMAINS: [&str; 18] = [
    "project_gutenberg", "wikipedia", "wikinews", "plos", "us_federal_register", "fineweb", "global_voices", "fineweb_blogs", "reddit",
    "ubuntu_irc", "wikipedia_talk", "wikisource", "instructional", "peer_review", "legal_opinions", "spoken_transcripts", "student_essays",
    "historical_manuscripts",
];

/// Collection -> protocol evaluation domain (splits-v4 `evaluation_domains`
/// collections and protocol `human_sources` evaluation uses). Collections
/// without an evaluation use map to None. Applies to every evaluation split
/// (Dev-iter, Dev-select, CAL, Test); on the seven original domains it agrees
/// with `domain_of`, so earlier evaluation plans are unchanged.
pub fn eval_domain_of(collection: &str) -> Option<&'static str> {
    Some(match collection {
        "project_gutenberg_historical" => "project_gutenberg",
        "wikipedia_enwiki_historical" => "wikipedia",
        "wikinews_enwikinews_historical" => "wikinews",
        "plos_historical" => "plos",
        "us_federal_register_historical" => "us_federal_register",
        "fineweb_cc_historical" => "fineweb",
        "global_voices_historical" => "global_voices",
        "fineweb_blog_cc_historical" => "fineweb_blogs",
        "reddit_webis_tldr17_historical" => "reddit",
        "ubuntu_irc_historical" => "ubuntu_irc",
        "wikipedia_talk_wikiconv_historical" => "wikipedia_talk",
        "wikisource_enwikisource_historical" => "wikisource",
        "wikivoyage_enwikivoyage_historical" | "wikibooks_cookbook_historical" | "wikibooks_enwikibooks_historical" => "instructional",
        "openreview_orb_reviews_historical" => "peer_review",
        "caselaw_access_project_historical" | "courtlistener_opinions_historical" => "legal_opinions",
        "fomc_transcripts_historical" | "ami_meeting_corpus_manual_transcripts" => "spoken_transcripts",
        "persuade_2_0_student_essays" | "ellipse_ell_student_essays" => "student_essays",
        "loc_by_the_people_historical" => "historical_manuscripts",
        _ => return None,
    })
}

/// Protocol mirror registers for the 18 evaluation domains, verbatim.
pub fn eval_register(domain: &str) -> Option<&'static str> {
    Some(match domain {
        "project_gutenberg" => "a passage from a published book",
        "wikipedia" => "a section of an encyclopedia article",
        "wikinews" => "a news report",
        "plos" => "a passage from a scientific research article",
        "us_federal_register" => "a passage from a US federal government notice or rule",
        "fineweb" => "a passage from a web page",
        "global_voices" => "an article from an international citizen-media news site",
        "fineweb_blogs" => "a post from a personal blog",
        "reddit" => "a post on an online discussion forum",
        "ubuntu_irc" => "a set of one person's messages in a technical-support chat channel",
        "wikipedia_talk" => "a comment on an encyclopedia article's discussion page",
        "wikisource" => "a passage from a historical published text",
        "instructional" => "a passage from a travel guide, recipe or how-to guide",
        "peer_review" => "a peer review of a scientific paper",
        "legal_opinions" => "a passage from a court opinion",
        "spoken_transcripts" => "a speaker's turn from a meeting transcript",
        "student_essays" => "an argumentative essay by a secondary-school student",
        "historical_manuscripts" => "a passage from a historical letter, diary or personal paper",
        _ => return None,
    })
}

/// Register used in a split's mirror prompts: Train keeps its recorded
/// registers (Train plans stay byte-identical); evaluation splits use the
/// protocol registers.
pub fn split_register(split: &str, domain: &str) -> &'static str {
    if split == "train" { register(domain) } else { eval_register(domain).unwrap_or_else(|| register(domain)) }
}

/// Collection -> domain. Evaluation domains and their registers follow
/// protocol v1.2; Train-only collections get their own domain label.
pub fn domain_of(collection: &str) -> Result<&'static str> {
    Ok(match collection {
        "project_gutenberg_historical" => "project_gutenberg",
        "wikipedia_enwiki_historical" => "wikipedia",
        "wikinews_enwikinews_historical" => "wikinews",
        "plos_historical" => "plos",
        "us_federal_register_historical" => "us_federal_register",
        "fineweb_cc_historical" => "fineweb",
        "global_voices_historical" => "global_voices",
        "fineweb_blog_cc_historical" => "fineweb_blogs",
        "plos_blog_historical" => "plos_blogs",
        "reddit_webis_tldr17_historical" => "reddit",
        "ubuntu_irc_historical" => "ubuntu_irc",
        "wikipedia_talk_wikiconv_historical" => "wikipedia_talk",
        "wikisource_enwikisource_historical" => "wikisource",
        "wikivoyage_enwikivoyage_historical" | "wikibooks_cookbook_historical" | "wikibooks_enwikibooks_historical" => "instructional",
        "openreview_orb_reviews_historical" | "f1000research_reports_historical" => "peer_review",
        "caselaw_access_project_historical" | "courtlistener_opinions_historical" => "legal_opinions",
        "amazon_reviews_2023_historical" => "consumer_reviews",
        "american_stories_historical" | "bl_hmd_newspapers_historical" => "historical_newspapers",
        "ami_meeting_corpus_manual_transcripts" | "icsi_meeting_corpus_transcripts" => "spoken_meetings",
        "fomc_transcripts_historical" | "govinfo_congressional_hearings_historical" | "govinfo_presidential_documents_historical" => "government_speech",
        "loc_by_the_people_historical" => "historical_manuscripts",
        "persuade_2_0_student_essays" => "student_essays",
        "stackexchange_historical" => "stackexchange",
        _ => bail!("unadmitted collection {collection}"),
    })
}

/// The planner domain of enron-v1's Train half (protocol A2 §6.1; B5b part
/// 2's D6 plan). Not in [`DOMAINS`]: no plan v1, v2 or v3 IDF table holds it.
pub const ENRON_DOMAIN: &str = "enron_email";

/// Collection -> domain for the Train inputs of the Train-only mirror
/// planner's plans only (`train_mirror`; B5b part 2; protocol A2 amendment
/// 27): [`domain_of`], plus enron-v1's Train half as [`ENRON_DOMAIN`]
/// (§6.1, V23). Every other Train reader keeps [`domain_of`], which refuses
/// enron-v1 as an unadmitted collection, so no style, brief or library
/// reader can take an Enron row into a prompt (§2.7: Enron text never goes
/// to a hosted model).
pub fn train_only_domain_of(collection: &str) -> Result<&'static str> {
    if collection == grammar_a_pools::pile::ENRON_COLLECTION { Ok(ENRON_DOMAIN) } else { domain_of(collection) }
}

pub const DOMAINS: [&str; 23] = [
    "project_gutenberg", "wikipedia", "wikinews", "plos", "us_federal_register", "fineweb", "global_voices", "fineweb_blogs", "plos_blogs",
    "reddit", "ubuntu_irc", "wikipedia_talk", "wikisource", "instructional", "peer_review", "legal_opinions", "consumer_reviews",
    "historical_newspapers", "spoken_meetings", "government_speech", "historical_manuscripts", "student_essays", "stackexchange",
];

pub fn register(domain: &str) -> &'static str {
    match domain {
        "project_gutenberg" => "a passage from a published book",
        "wikipedia" => "a section of an encyclopedia article",
        "wikinews" => "a news report",
        "plos" => "a passage from a scientific research article",
        "us_federal_register" => "a passage from a US federal government notice or rule",
        "fineweb" => "a passage from a web page",
        "global_voices" => "an article from an international citizen-media news site",
        "fineweb_blogs" => "a post from a personal blog",
        "reddit" => "a post on an online discussion forum",
        "ubuntu_irc" => "a set of one person's messages in a technical-support chat channel",
        "wikipedia_talk" => "a comment on an encyclopedia article's discussion page",
        "wikisource" => "a passage from a historical published text",
        "instructional" => "a passage from a travel guide, recipe or how-to guide",
        "peer_review" => "a peer review of a scientific paper",
        // Train-only domains (not protocol evaluation domains)
        "plos_blogs" => "a post from a science blog",
        "legal_opinions" => "a passage from a court opinion",
        "consumer_reviews" => "a customer product review",
        "historical_newspapers" => "an article from a historical newspaper",
        "spoken_meetings" => "a passage from a transcript of a work meeting",
        "government_speech" => "a passage from a transcript of government remarks or testimony",
        "historical_manuscripts" => "a passage from a historical letter or diary",
        "spoken_transcripts" => "a speaker's turn from a meeting transcript",
        "student_essays" => "a student essay",
        "stackexchange" => "an answer on a question-and-answer website",
        ENRON_DOMAIN => "an email message written by an employee of a company",
        _ => unreachable!("register for {domain}"),
    }
}

pub fn key(x: &str, id: &str) -> String {
    text::sha_hex(format!("{PLAN_KEY_PREFIX}{x}:{id}").as_bytes())
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Decoding {
    pub profile: String,
    pub temperature: f64,
    pub top_p: f64,
    pub top_k: u32,
    pub min_p: f64,
    pub repeat_penalty: f64,
    pub max_tokens: u32,
}

pub fn profile(name: &str, t: f64, p: f64, max_tokens: u32) -> Decoding {
    Decoding { profile: name.into(), temperature: t, top_p: p, top_k: 0, min_p: 0.0, repeat_penalty: 1.0, max_tokens }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Item {
    pub item_id: String,
    pub split: String,
    pub domain: String,
    pub parent_id: String,
    pub parent_text_sha256: String,
    pub source_group: String,
    /// primary_mirror | primary_mixed | train_base_robustness | train_low_share
    pub cohort: String,
    /// mirror | span | full | continuation | splice
    pub kind: String,
    pub bin: Option<String>,
    pub target_bp: Option<u32>,
    pub anchor: Option<String>,
    pub multi_span: bool,
    pub generator: String,
    pub decoding: Decoding,
    pub template_id: String,
    /// Removed source byte spans, ascending; empty for mirrors.
    pub source_spans: Vec<(usize, usize)>,
    pub requested_fraction: Option<f64>,
    pub requests: Vec<String>,
    pub notes: Vec<String>,
    pub ranks: BTreeMap<String, usize>,
    /// Plan v2 diverse cohorts: rewrite operation (none for v1 items).
    #[serde(default)]
    pub operation: Option<String>,
    /// Plan v2: brief id, document type, decoding sample and variants.
    #[serde(default)]
    pub style: Option<Value>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Request {
    pub request_id: String,
    pub item_id: String,
    pub parent_id: String,
    pub split: String,
    pub generator: String,
    /// chat | completion
    pub endpoint: String,
    pub template_id: String,
    pub prompt: String,
    pub span_index: usize,
    pub source_span: Option<(usize, usize)>,
    pub target_words: usize,
    /// Base continuations/splices: word budget for the cut.
    pub cut_budget: Option<usize>,
    pub decoding: Decoding,
    pub request_sha256: String,
    pub seed: u32,
    /// Plan v2 rewrite operation (absent in v1 requests).
    #[serde(default)]
    pub op: Option<String>,
    /// Item cohort (plan v2), used to run core cohorts before supplementary ones.
    #[serde(default)]
    pub cohort: Option<String>,
    /// The attempts this request gets, counted from its first (B5b; ruling
    /// R6, protocol A2 amendment 20 item 20.5): absent on every planned
    /// request, which gets `generation::ATTEMPTS`; a regenerated mirror's new
    /// request carries the item's remaining attempts. Absent is not
    /// serialised, so existing request bytes and `replan-check` are unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempt_limit: Option<u32>,
}

impl Request {
    /// The request's attempt limit: `attempt_limit`, else
    /// `generation::ATTEMPTS` (R0's two per request).
    pub fn attempts(&self) -> u32 {
        self.attempt_limit.unwrap_or(crate::generation::ATTEMPTS)
    }
}

pub struct Human {
    pub split: String,
    pub domain: &'static str,
    pub id: String,
    pub text: String,
    pub text_sha256: String,
    pub word_count: usize,
    pub title: Option<String>,
    pub source_group: String,
    /// Row genre label (`domain` field: fiction, encyclopedia, science, ...).
    pub genre: String,
}

pub fn human_from(row: &Value, split: &str) -> Result<Human> {
    human_from_in(row, split, false)
}

/// [`human_from`]; with `train_only` a Train row's domain is
/// [`train_only_domain_of`]'s (the Train-only mirror planner's plans, whose
/// `generate` and `finalize` read their Train input this way).
pub fn human_from_in(row: &Value, split: &str, train_only: bool) -> Result<Human> {
    let fs = row["frozen_split"].as_str().context("frozen_split")?;
    ensure!(fs == split, "row {} has frozen_split {fs}, expected {split}", row["id"]);
    ensure!(row["origin"] == "human_only", "human source required");
    let text = row["text"].as_str().context("text")?.to_owned();
    ensure!(text::sha_hex(text.as_bytes()) == row["text_sha256"].as_str().unwrap_or(""), "text digest mismatch");
    let wc = text::words(&text);
    ensure!(row["word_count"].as_u64() == Some(wc as u64), "word count mismatch");
    let collection = row["collection"].as_str().context("collection")?;
    let domain = if split == "train" {
        if train_only { train_only_domain_of(collection)? } else { domain_of(collection)? }
    } else {
        eval_domain_of(collection).with_context(|| format!("collection {collection} has no evaluation domain ({split})"))?
    };
    let prov = &row["provenance"];
    let title = prov["title"]
        .as_str()
        .or_else(|| prov["subject"].as_str())
        .map(|t| t.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|t| !t.is_empty());
    Ok(Human {
        split: split.into(),
        domain,
        id: row["id"].as_str().context("id")?.into(),
        text,
        text_sha256: row["text_sha256"].as_str().unwrap().into(),
        word_count: wc,
        title,
        source_group: row["source_group"].as_str().context("source_group")?.into(),
        genre: row["domain"].as_str().context("genre")?.into(),
    })
}

/// Ranks of `k_x(d)` within (split, domain), ascending key order.
pub fn ranks(humans: &[Human], x: &str) -> HashMap<String, usize> {
    let mut by: BTreeMap<(&str, &str), Vec<(String, &str)>> = BTreeMap::new();
    for h in humans {
        by.entry((h.split.as_str(), h.domain)).or_default().push((key(x, &h.id), h.id.as_str()));
    }
    let mut out = HashMap::new();
    for (_, mut v) in by {
        v.sort();
        for (r, (_, id)) in v.into_iter().enumerate() {
            out.insert(id.to_owned(), r);
        }
    }
    out
}

pub fn anchor_of(id: &str, bin: &str) -> usize {
    Sha256::digest(format!("{id}:{bin}").as_bytes())[0] as usize % 3
}
pub const ANCHORS: [&str; 3] = ["start", "middle", "end"];

pub fn bin_target_bp(bin: &str) -> Option<u32> {
    match bin {
        "R0" => Some(500),
        "R1" => Some(1750),
        "R2" => Some(3750),
        "R3" => Some(6250),
        "R4" => Some(8250),
        _ => None,
    }
}

pub fn prefix_words(s: &[Sentence], text: &str) -> Vec<usize> {
    // cumulative words before sentence i (len n+1)
    let mut c = vec![0];
    for x in s {
        c.push(c.last().unwrap() + text::words(&text[x.start..x.end]));
    }
    c
}

/// One contiguous whole-sentence run (sentence indices [i, j)) whose word
/// fraction is closest to `target_bp`, shorter on ties, anchored at the
/// start (i = 0), end (j = n) or middle (0 < i, j < n, run midpoint closest
/// to the document midpoint). Never the whole document.
pub fn single_run(cum: &[usize], target_bp: u32, anchor: usize) -> Option<(usize, usize)> {
    let n = cum.len() - 1;
    let total = cum[n] as i64;
    if n < 2 || total == 0 {
        return None;
    }
    let mut cands = Vec::new();
    for i in 0..n {
        for j in i + 1..=n {
            if i == 0 && j == n {
                continue;
            }
            let ok = match anchor {
                0 => i == 0,
                2 => j == n,
                _ => i > 0 && j < n,
            };
            if ok {
                cands.push((i, j));
            }
        }
    }
    if cands.is_empty() {
        // middle anchor on a two-sentence document: fall back to any run
        for i in 0..n {
            for j in i + 1..=n {
                if !(i == 0 && j == n) {
                    cands.push((i, j));
                }
            }
        }
    }
    cands.into_iter().min_by_key(|&(i, j)| {
        let w = (cum[j] - cum[i]) as i64;
        let diff = (w * 10000 - target_bp as i64 * total).abs();
        let mid = if anchor == 1 { (2 * cum[i] as i64 + w - total).abs() } else { 0 };
        (diff, w, mid, i)
    })
}

/// Two runs of half the target each, one at the start and one at the end,
/// with at least one human sentence between. None when that cannot fit.
pub fn two_runs(cum: &[usize], target_bp: u32) -> Option<((usize, usize), (usize, usize))> {
    let n = cum.len() - 1;
    let total = cum[n] as i64;
    if n < 3 {
        return None;
    }
    let half = target_bp as i64; // compare w*20000 with target*total
    let best = |cands: Vec<(usize, usize)>| {
        cands.into_iter().min_by_key(|&(i, j)| {
            let w = (cum[j] - cum[i]) as i64;
            ((w * 20000 - half * total).abs(), w, i)
        })
    };
    let a = best((1..n).map(|j| (0, j)).collect())?;
    let b = best((1..n).map(|i| (i, n)).collect())?;
    if a.1 < b.0 { Some((a, b)) } else { None }
}

/// R5: replace every sentence except one kept at start/middle/end.
pub fn r5_runs(cum: &[usize], anchor: usize) -> Option<Vec<(usize, usize)>> {
    let n = cum.len() - 1;
    if n < 2 {
        return None;
    }
    let total = cum[n];
    let keep = match anchor {
        0 => 0,
        2 => n - 1,
        _ => {
            let mut k = 0;
            while k < n && cum[k + 1] * 2 <= total {
                k += 1;
            }
            let k = k.min(n - 1);
            if n >= 3 { k.clamp(1, n - 2) } else { k }
        }
    };
    let mut v = Vec::new();
    if keep > 0 {
        v.push((0, keep));
    }
    if keep + 1 < n {
        v.push((keep + 1, n));
    }
    Some(v)
}

pub fn byte_span(s: &[Sentence], run: (usize, usize)) -> (usize, usize) {
    (s[run.0].start, s[run.1 - 1].end)
}

pub fn request_sha(generator: &str, endpoint: &str, template: &str, prompt: &str, d: &Decoding) -> String {
    let v = json!({"generator":generator,"endpoint":endpoint,"template_id":template,"prompt":prompt,"decoding":d});
    text::sha_hex(&serde_json::to_vec(&v).unwrap())
}

pub fn seed_of(request_sha: &str) -> u32 {
    let b = hex::decode(&request_sha[..8]).unwrap();
    u32::from_be_bytes([b[0], b[1], b[2], b[3]])
}

pub fn mirror_prompt(h: &Human, terms: &[String]) -> String {
    mirror_prompt_words(h, terms, h.word_count)
}

/// The mirror prompt asking for `words` words: [`mirror_prompt`] at the
/// parent's length, or a new mirror at `mirror_target_words_v2` (B5b part 2).
pub fn mirror_prompt_words(h: &Human, terms: &[String], words: usize) -> String {
    let title = h.title.as_ref().map(|t| format!(" Title or subject: {t}.")).unwrap_or_default();
    format!(
        "Write {} in English, about {} words long.{} Cover these key terms: {}. Write natural, original prose. Return only the passage, with no title, heading, preface, analysis, or explanation.",
        split_register(&h.split, h.domain),
        words,
        title,
        terms.join(", ")
    )
}

pub fn rewrite_prompt(span: &str) -> String {
    format!("{REWRITE_INSTRUCTION}\n\n{span}")
}

/// `mirror_target_words_v2` (protocol A2 §6.3; B5b): the requested length of
/// every new or regenerated mirror in every population.
pub const MIRROR_TARGET_V2_ID: &str = "mirror_target_words_v2";
/// The 150 of §6.3: the final-document window's lower bound
/// (`grammar_a_form::window::LENGTH_WINDOW`, a test pins it).
pub const MIRROR_TARGET_V2_FLOOR: u128 = 150;
/// The rule as records state it.
pub const MIRROR_TARGET_V2_RULE: &str = "mirror_target_words_v2 (protocol A2 section 6.3): requested words = max(parent words, ceil(150 / r05)), parent words in whitespace words (text::words), r05 the S0-6 value read as an exact decimal or ratio and ceil computed in integers, never in floating point";

/// r05 (S0-6, `VALUES-R2.json`) as the exact positive rational `num / den`:
/// a decimal (`0.8125`, digits only, no sign or exponent) or a ratio of
/// positive integers (`13/16`). Kept as given for records.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct R05 {
    num: u128,
    den: u128,
    text: String,
}

impl R05 {
    /// At most 30 digits in all, so `150 * den` cannot overflow.
    pub fn parse(s: &str) -> Result<R05> {
        let t = s.trim();
        let digits = |x: &str| !x.is_empty() && x.bytes().all(|b| b.is_ascii_digit());
        let (num, den) = if let Some((a, b)) = t.split_once('/') {
            ensure!(digits(a) && digits(b) && a.len() + b.len() <= 30, "r05 {s:?}: a ratio is two positive integers a/b");
            (a.parse::<u128>()?, b.parse::<u128>()?)
        } else {
            let (i, f) = t.split_once('.').unwrap_or((t, ""));
            ensure!(digits(i) && (f.is_empty() || digits(f)) && i.len() + f.len() <= 30 && !(t.ends_with('.')),
                "r05 {s:?}: a decimal is digits with an optional fraction (no sign or exponent)");
            (format!("{i}{f}").parse::<u128>()?, 10u128.pow(f.len() as u32))
        };
        ensure!(num > 0 && den > 0, "r05 {s:?} must be positive");
        Ok(R05 { num, den, text: t.to_string() })
    }
    /// ceil(150 / r05) = ceil(150 * den / num), in integers.
    pub fn floor_words(&self) -> usize {
        (MIRROR_TARGET_V2_FLOOR * self.den).div_ceil(self.num) as usize
    }
    pub fn as_str(&self) -> &str {
        &self.text
    }
    /// The value as records hold it: as given, the exact ratio and the floor.
    pub fn record(&self) -> Value {
        json!({"r05": self.text, "r05_ratio": [self.num.to_string(), self.den.to_string()], "floor_words": self.floor_words(),
            "rule": MIRROR_TARGET_V2_RULE})
    }
}

/// `mirror_target_words_v2`: max(parent words, ceil(150 / r05)).
pub fn mirror_target_words_v2(parent_words: usize, r05: &R05) -> usize {
    parent_words.max(r05.floor_words())
}

pub struct Plan {
    pub items: Vec<Item>,
    pub requests: Vec<Request>,
    pub failures: Vec<Value>,
}

pub fn eval_generator(split: &str, slot: usize) -> &'static str {
    match split {
        "cal" => {
            if slot < 7 {
                CAL_ONLY
            } else {
                FAMILY_ORDER[(slot - 7) / 2]
            }
        }
        "test" => {
            if slot < 7 {
                TEST_ONLY_INSTRUCT
            } else {
                FAMILY_ORDER[(slot - 7) / 2]
            }
        }
        "dev_iter" | "dev_select" => FAMILY_ORDER[slot / 3],
        _ => unreachable!(),
    }
}

/// Train decoding profiles (declared range): rank(k_decoding) mod 4.
pub fn train_decoding(r: usize) -> Decoding {
    match r % 4 {
        0 => profile("A", 0.7, 0.9, 1536),
        1 => profile("B", 1.0, 0.95, 1536),
        2 => profile("C", 0.8, 0.92, 1536),
        _ => profile("D", 0.9, 1.0, 1536),
    }
}
pub fn eval_decoding(r: usize) -> Decoding {
    if r % 2 == 0 { profile("A", 0.7, 0.9, 1536) } else { profile("B", 1.0, 0.95, 1536) }
}

pub fn build(humans: &[Human], idf: &HashMap<&'static str, Idf>) -> Result<Plan> {
    let stop = text::stopwords();
    let names = [
        "mirror_gen", "mixed_gen", "mixed_bin", "multi_span", "decoding", "train_base", "train_low", "train_low_gen",
    ];
    let rk: HashMap<&str, HashMap<String, usize>> = names.iter().map(|x| (*x, ranks(humans, x))).collect();
    let mut items = Vec::new();
    let mut requests = Vec::new();
    let mut failures = Vec::new();
    for h in humans {
        let r = |x: &str| rk[x][&h.id];
        let train = h.split == "train";
        let dec = if train { train_decoding(r("decoding")) } else { eval_decoding(r("decoding")) };
        let sents = text::sentences(&h.text);
        let cum = prefix_words(&sents, &h.text);
        ensure!(cum[sents.len()] == h.word_count, "sentence word accounting for {}", h.id);
        let mut ranks_rec = BTreeMap::new();
        for x in names {
            ranks_rec.insert(x.to_string(), r(x));
        }
        let new_item = |cohort: &str, kind: &str, generator: &str, tmpl: &str, d: &Decoding| Item {
            item_id: format!("pos-{}", &text::sha_hex(format!("{}:{}:{}", h.split, h.id, cohort).as_bytes())[..24]),
            split: h.split.clone(),
            domain: h.domain.into(),
            parent_id: h.id.clone(),
            parent_text_sha256: h.text_sha256.clone(),
            source_group: h.source_group.clone(),
            cohort: cohort.into(),
            kind: kind.into(),
            bin: None,
            target_bp: None,
            anchor: None,
            multi_span: false,
            generator: generator.into(),
            decoding: d.clone(),
            template_id: tmpl.into(),
            source_spans: vec![],
            requested_fraction: None,
            requests: vec![],
            notes: vec![],
            ranks: ranks_rec.clone(),
            operation: None,
            style: None,
        };
        let mut push_req = |item: &mut Item, endpoint: &str, prompt: String, span: Option<(usize, usize)>, target: usize, budget: Option<usize>| {
            let rs = request_sha(&item.generator, endpoint, &item.template_id, &prompt, &item.decoding);
            let req = Request {
                request_id: format!("req-{}", &text::sha_hex(format!("{}:{}", item.item_id, item.requests.len()).as_bytes())[..24]),
                item_id: item.item_id.clone(),
                parent_id: item.parent_id.clone(),
                split: item.split.clone(),
                generator: item.generator.clone(),
                endpoint: endpoint.into(),
                template_id: item.template_id.clone(),
                prompt,
                span_index: item.requests.len(),
                source_span: span,
                target_words: target,
                cut_budget: budget,
                decoding: item.decoding.clone(),
                seed: seed_of(&rs),
                request_sha256: rs,
                op: None,
                cohort: Some(item.cohort.clone()),
                attempt_limit: None,
            };
            item.requests.push(req.request_id.clone());
            requests.push(req);
        };
        // Mirror
        let mg = if train { TRAIN_INSTRUCT[r("mirror_gen") % 8] } else { eval_generator(&h.split, r("mirror_gen") % 21) };
        let mut it = new_item("primary_mirror", "mirror", mg, MIRROR_TEMPLATE_ID, &dec);
        let terms = text::key_terms(&h.text, &idf[h.domain], &stop, 8);
        let prompt = mirror_prompt(h, &terms);
        push_req(&mut it, "chat", prompt, None, h.word_count, None);
        items.push(it);
        // Mixed
        let xg = if train { TRAIN_INSTRUCT[r("mixed_gen") % 8] } else { eval_generator(&h.split, r("mixed_gen") % 21) };
        let bin = ["R1", "R2", "R3", "R4", "R5", "R6"][r("mixed_bin") % 6];
        let mut it = new_item("primary_mixed", if bin == "R6" { "full" } else { "span" }, xg, REWRITE_TEMPLATE_ID, &dec);
        plan_mixed(&mut it, h, &sents, &cum, bin, r("multi_span") % 3 == 0, &mut failures);
        if !it.source_spans.is_empty() {
            for &(s, e) in &it.source_spans.clone() {
                let span_text = &h.text[s..e];
                push_req(&mut it, "chat", rewrite_prompt(span_text), Some((s, e)), text::words(span_text), None);
            }
            items.push(it);
        }
        if train {
            // Train-only supplementary cohorts (not in the gated plan).
            if r("train_low") % 4 == 0 {
                let g = TRAIN_INSTRUCT[r("train_low_gen") % 8];
                let mut it = new_item("train_low_share", "span", g, REWRITE_TEMPLATE_ID, &dec);
                plan_mixed(&mut it, h, &sents, &cum, "R0", false, &mut failures);
                if !it.source_spans.is_empty() {
                    for &(s, e) in &it.source_spans.clone() {
                        let span_text = &h.text[s..e];
                        push_req(&mut it, "chat", rewrite_prompt(span_text), Some((s, e)), text::words(span_text), None);
                    }
                    items.push(it);
                }
            }
            let rb = r("train_base");
            if rb % 4 == 0 {
                let g = TRAIN_BASE[(rb / 8) % 3];
                let cont = (rb / 4) % 2 == 0;
                let bd = profile("base", 0.8, 0.95, 1024);
                let kind = if cont { "continuation" } else { "splice" };
                let tmpl = if cont { BASE_CONT_TEMPLATE_ID } else { BASE_SPLICE_TEMPLATE_ID };
                let mut it = new_item("train_base_robustness", kind, g, tmpl, &bd);
                let n = sents.len();
                let total = h.word_count;
                let plan = base_runs(&cum, cont);
                match plan {
                    Some((a, b)) => {
                        let (s, e) = (sents[a].start, sents[b - 1].end);
                        let removed = text::words(&h.text[s..e]);
                        let prefix = h.text[..sents[a - 1].end].to_owned();
                        let budget = if cont { total - cum[a] } else { removed };
                        it.source_spans = vec![(s, e)];
                        it.requested_fraction = Some(removed as f64 / total as f64);
                        push_req(&mut it, "completion", prefix, Some((s, e)), removed, Some(budget));
                        items.push(it);
                    }
                    None => failures.push(json!({"item_id":it.item_id,"split":h.split,"parent_id":h.id,"cohort":it.cohort,
                        "generator":g,"kind":kind,"stage":"plan","reason":"no_valid_prefix_suffix_plan","sentences":n})),
                }
            }
        }
    }
    Ok(Plan { items, requests, failures })
}

/// Base-checkpoint plan over whole sentences: the generated run [a, b).
/// Continuation: prefix = shortest whole-sentence prefix with at least 30%
/// of the words, generated run = the rest. Splice: prefix with at least 25%,
/// suffix = shortest whole-sentence suffix with at least 25%, generated run =
/// the middle (non-empty). None when no such plan exists.
pub fn base_runs(cum: &[usize], cont: bool) -> Option<(usize, usize)> {
    let n = cum.len() - 1;
    let total = cum[n];
    let need = if cont { 30 } else { 25 };
    let j = (1..=n).find(|&j| cum[j] * 100 >= need * total);
    match j {
        Some(j) if j < n => {
            if cont {
                Some((j, n))
            } else {
                let i2 = (0..n).rev().find(|&i| (total - cum[i]) * 100 >= 25 * total);
                match i2 {
                    Some(i2) if j < i2 => Some((j, i2)),
                    _ => None,
                }
            }
        }
        _ => None,
    }
}

/// Robustness decoding (protocol `robustness_test_only.decoding`); fixed
/// settings as for every evaluation request.
pub fn robustness_decoding() -> Decoding {
    profile("robust", 0.8, 0.95, 1024)
}

/// Test plan (protocol-v1.json `positive_plan`, Test cycle): the two primary
/// requests per human under the Test 21-slot cycle (OLMo-2 instruct in slots
/// 0-6), and the Test-only robustness slice: humans with
/// rank(k_robust) mod 2 == 0 get one OLMo-2 base document, continuation when
/// floor(rank(k_robust) / 2) mod 2 == 0, else splice. Ranks are within
/// (test, domain). Every human must be a Test human of an evaluation domain.
pub fn build_test(humans: &[Human], idf: &HashMap<&'static str, Idf>) -> Result<Plan> {
    for h in humans {
        ensure!(h.split == "test", "test plan given a {} human {}", h.split, h.id);
        ensure!(EVAL_DOMAINS.contains(&h.domain), "test human {} has non-evaluation domain {}", h.id, h.domain);
    }
    let mut p = build(humans, idf)?;
    let rr = ranks(humans, "robust");
    for h in humans {
        let rb = rr[&h.id];
        if rb % 2 != 0 {
            continue;
        }
        let cont = (rb / 2) % 2 == 0;
        let kind = if cont { "continuation" } else { "splice" };
        let tmpl = if cont { BASE_CONT_TEMPLATE_ID } else { BASE_SPLICE_TEMPLATE_ID };
        let d = robustness_decoding();
        let item_id = format!("pos-{}", &text::sha_hex(format!("{}:{}:{}", h.split, h.id, TEST_ROBUSTNESS_COHORT).as_bytes())[..24]);
        let sents = text::sentences(&h.text);
        let cum = prefix_words(&sents, &h.text);
        ensure!(cum[sents.len()] == h.word_count, "sentence word accounting for {}", h.id);
        let mut it = Item {
            item_id: item_id.clone(),
            split: h.split.clone(),
            domain: h.domain.into(),
            parent_id: h.id.clone(),
            parent_text_sha256: h.text_sha256.clone(),
            source_group: h.source_group.clone(),
            cohort: TEST_ROBUSTNESS_COHORT.into(),
            kind: kind.into(),
            bin: None,
            target_bp: None,
            anchor: None,
            multi_span: false,
            generator: TEST_ONLY_BASE.into(),
            decoding: d.clone(),
            template_id: tmpl.into(),
            source_spans: vec![],
            requested_fraction: None,
            requests: vec![],
            notes: vec![],
            ranks: BTreeMap::from([("robust".to_string(), rb)]),
            operation: None,
            style: None,
        };
        match base_runs(&cum, cont) {
            Some((a, b)) => {
                let (s, e) = (sents[a].start, sents[b - 1].end);
                let removed = text::words(&h.text[s..e]);
                let prompt = h.text[..sents[a - 1].end].to_owned();
                // continuation: keep the document within the human word count;
                // splice: the middle may not exceed the removed words. Both
                // equal the removed words here.
                let budget = removed;
                it.source_spans = vec![(s, e)];
                it.requested_fraction = Some(removed as f64 / h.word_count as f64);
                let rs = request_sha(&it.generator, "completion", &it.template_id, &prompt, &it.decoding);
                let req = Request {
                    request_id: format!("req-{}", &text::sha_hex(format!("{}:{}", it.item_id, 0).as_bytes())[..24]),
                    item_id: it.item_id.clone(),
                    parent_id: it.parent_id.clone(),
                    split: it.split.clone(),
                    generator: it.generator.clone(),
                    endpoint: "completion".into(),
                    template_id: it.template_id.clone(),
                    prompt,
                    span_index: 0,
                    source_span: Some((s, e)),
                    target_words: removed,
                    cut_budget: Some(budget),
                    decoding: it.decoding.clone(),
                    seed: seed_of(&rs),
                    request_sha256: rs,
                    op: None,
                    cohort: Some(it.cohort.clone()),
                    attempt_limit: None,
                };
                it.requests.push(req.request_id.clone());
                p.requests.push(req);
                p.items.push(it);
            }
            None => p.failures.push(json!({"item_id":item_id,"split":h.split,"parent_id":h.id,"cohort":TEST_ROBUSTNESS_COHORT,
                "generator":TEST_ONLY_BASE,"kind":kind,"stage":"plan","reason":"no_valid_prefix_suffix_plan","sentences":sents.len()})),
        }
    }
    Ok(p)
}

/// Protocol Test quotas for a domain with n humans: OLMo-2 instruct mirror
/// (= mixed) requests 7*floor(n/21) + min(n mod 21, 7), robustness ceil(n/2),
/// frontier ceil(n/4).
pub fn test_quota(n: usize) -> (usize, usize, usize) {
    (7 * (n / 21) + (n % 21).min(7), n.div_ceil(2), n.div_ceil(4))
}

pub fn plan_mixed(it: &mut Item, h: &Human, sents: &[Sentence], cum: &[usize], bin: &str, multi: bool, failures: &mut Vec<Value>) {
    it.bin = Some(bin.into());
    let anchor = anchor_of(&h.id, bin);
    let n = sents.len();
    let runs: Option<Vec<(usize, usize)>> = match bin {
        "R6" => {
            it.source_spans = vec![(0, h.text.len())];
            it.requested_fraction = Some(1.0);
            return;
        }
        "R5" => {
            it.anchor = Some(ANCHORS[anchor].into());
            r5_runs(cum, anchor)
        }
        _ => {
            let t = bin_target_bp(bin).unwrap();
            it.target_bp = Some(t);
            let two = if multi && matches!(bin, "R2" | "R3" | "R4") { Some(two_runs(cum, t)) } else { None };
            match two {
                Some(Some((a, b))) => {
                    it.multi_span = true;
                    it.anchor = Some("start+end".into());
                    Some(vec![a, b])
                }
                Some(None) => {
                    it.notes.push("two_span_did_not_fit_single_run_used".into());
                    it.anchor = Some(ANCHORS[anchor].into());
                    single_run(cum, t, anchor).map(|r| vec![r])
                }
                None => {
                    it.anchor = Some(ANCHORS[anchor].into());
                    single_run(cum, t, anchor).map(|r| vec![r])
                }
            }
        }
    };
    match runs {
        Some(v) if !v.is_empty() => {
            let spans: Vec<(usize, usize)> = v.iter().map(|&r| byte_span(sents, r)).collect();
            let w: usize = v.iter().map(|&(i, j)| cum[j] - cum[i]).sum();
            it.requested_fraction = Some(w as f64 / cum[n] as f64);
            it.source_spans = spans;
        }
        _ => failures.push(json!({"item_id":it.item_id,"split":h.split,"parent_id":h.id,"cohort":it.cohort,
            "generator":it.generator,"bin":bin,"stage":"plan","reason":"no_valid_whole_sentence_span_plan","sentences":n})),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn cum(ws: &[usize]) -> Vec<usize> {
        let mut c = vec![0];
        for w in ws {
            c.push(c.last().unwrap() + w);
        }
        c
    }
    #[test]
    fn single_run_closest_shorter_on_tie_and_anchors() {
        let c = cum(&[10, 10, 10, 10, 10, 10, 10, 10, 10, 10]);
        assert_eq!(single_run(&c, 1750, 0), Some((0, 2))); // 20% vs 10%: 20 closer to 17.5
        assert_eq!(single_run(&c, 1500, 0), Some((0, 1))); // tie 10/20 -> shorter
        assert_eq!(single_run(&c, 3750, 2), Some((6, 10)));
        let m = single_run(&c, 3750, 1).unwrap();
        assert_eq!(m.1 - m.0, 4);
        assert!(m.0 > 0 && m.1 < 10);
        assert_eq!(m, (3, 7));
        assert_eq!(single_run(&cum(&[100]), 1750, 0), None);
    }
    #[test]
    fn two_runs_need_a_gap() {
        let c = cum(&[10; 10]);
        assert_eq!(two_runs(&c, 3750), Some(((0, 2), (8, 10))));
        assert_eq!(two_runs(&cum(&[10, 10]), 3750), None);
        assert_eq!(two_runs(&cum(&[50, 1, 50]), 8250), Some(((0, 1), (2, 3))));
    }
    #[test]
    fn r5_keeps_one_sentence() {
        let c = cum(&[10; 5]);
        assert_eq!(r5_runs(&c, 0), Some(vec![(1, 5)]));
        assert_eq!(r5_runs(&c, 2), Some(vec![(0, 4)]));
        assert_eq!(r5_runs(&c, 1), Some(vec![(0, 2), (3, 5)]));
        assert_eq!(r5_runs(&cum(&[10]), 1), None);
    }
    #[test]
    fn mirror_target_words_v2_is_exact() {
        assert_eq!(MIRROR_TARGET_V2_FLOOR as usize, grammar_a_form::window::LENGTH_WINDOW.0);
        // ceil(150 / r05) in integers: 150 / 0.6 is 250 exactly (f64 gives 250.00000000000003)
        for (r, want) in [("0.6", 250), ("0.75", 200), ("0.8125", 185), ("13/16", 185), ("1", 150), ("0.7", 215), ("2", 75), ("0.5", 300),
            ("0.333", 451), ("1/3", 450), ("150", 1), ("1000", 1)]
        {
            assert_eq!(R05::parse(r).unwrap().floor_words(), want, "{r}");
        }
        let r = R05::parse("0.8").unwrap();
        assert_eq!(r.floor_words(), 188);
        assert_eq!(mirror_target_words_v2(120, &r), 188);
        assert_eq!(mirror_target_words_v2(188, &r), 188);
        assert_eq!(mirror_target_words_v2(189, &r), 189);
        assert_eq!(mirror_target_words_v2(400, &r), 400);
        for bad in ["0", "0.0", "-0.5", "+0.5", "1e-1", ".5", "5.", "0x1", "", "a/b", "1/0", "0/3", "0.5.1", "1 /2", "0.1234567890123456789012345678901"] {
            assert!(R05::parse(bad).is_err(), "{bad}");
        }
        assert_eq!(R05::parse(" 0.8 ").unwrap().as_str(), "0.8");
    }
    #[test]
    fn keys_and_seed_are_stable() {
        assert_eq!(key("mirror_gen", "x"), text::sha_hex(b"slopninja-a-comp-plan-v1:mirror_gen:x"));
        let s = "0000000a".to_string() + &"0".repeat(56);
        assert_eq!(seed_of(&s), 10);
    }
    #[test]
    fn eval_slots_follow_protocol_cycle() {
        assert_eq!(eval_generator("cal", 0), "glm4");
        assert_eq!(eval_generator("cal", 6), "glm4");
        assert_eq!(eval_generator("cal", 7), "qwen36");
        assert_eq!(eval_generator("cal", 20), "ernie_instruct");
        assert_eq!(eval_generator("dev_iter", 0), "qwen36");
        assert_eq!(eval_generator("dev_select", 20), "ernie_instruct");
        let mut counts = HashMap::new();
        for s in 0..21 {
            *counts.entry(eval_generator("dev_iter", s)).or_insert(0) += 1;
        }
        assert!(counts.values().all(|&v| v == 3));
    }
    #[test]
    fn test_cycle_puts_olmo2_in_slots_0_to_6() {
        for s in 0..7 {
            assert_eq!(eval_generator("test", s), TEST_ONLY_INSTRUCT);
        }
        let fams: Vec<&str> = (7..21).map(|s| eval_generator("test", s)).collect();
        let want: Vec<&str> = FAMILY_ORDER.iter().flat_map(|f| [*f, *f]).collect();
        assert_eq!(fams, want);
        assert!(!(0..21).any(|s| eval_generator("test", s) == CAL_ONLY || eval_generator("test", s) == "gptoss20b"));
    }
    #[test]
    fn test_quotas_match_protocol_table() {
        // protocol-v1.json positive_plan.test_quotas.planned
        for (n, olmo, robust, frontier) in
            [(260, 91, 130, 65), (380, 128, 190, 95), (160, 56, 80, 40), (200, 70, 100, 50), (220, 77, 110, 55), (150, 52, 75, 38)]
        {
            assert_eq!(test_quota(n), (olmo, robust, frontier), "n = {n}");
        }
        // the rule equals the slot count of ranks 0..n
        for n in 0..100 {
            assert_eq!(test_quota(n).0, (0..n).filter(|r| r % 21 < 7).count());
            assert_eq!(test_quota(n).1, (0..n).filter(|r| r % 2 == 0).count());
            assert_eq!(test_quota(n).2, (0..n).filter(|r| r % 4 == 0).count());
        }
    }
    #[test]
    fn evaluation_domains_registers_and_mapping() {
        assert_eq!(eval_register("spoken_transcripts"), Some("a speaker's turn from a meeting transcript"));
        assert_eq!(eval_register("student_essays"), Some("an argumentative essay by a secondary-school student"));
        assert_eq!(eval_register("historical_manuscripts"), Some("a passage from a historical letter, diary or personal paper"));
        assert_eq!(eval_register("ubuntu_irc"), Some("a set of one person's messages in a technical-support chat channel"));
        assert_eq!(eval_register("plos_blogs"), None);
        let cols = [
            "project_gutenberg_historical", "wikipedia_enwiki_historical", "wikinews_enwikinews_historical", "plos_historical",
            "us_federal_register_historical", "fineweb_cc_historical", "global_voices_historical", "fineweb_blog_cc_historical",
            "reddit_webis_tldr17_historical", "ubuntu_irc_historical", "wikipedia_talk_wikiconv_historical",
            "wikisource_enwikisource_historical", "wikivoyage_enwikivoyage_historical", "wikibooks_cookbook_historical",
            "wikibooks_enwikibooks_historical", "openreview_orb_reviews_historical", "caselaw_access_project_historical",
            "courtlistener_opinions_historical", "fomc_transcripts_historical", "ami_meeting_corpus_manual_transcripts",
            "persuade_2_0_student_essays", "ellipse_ell_student_essays", "loc_by_the_people_historical",
        ];
        let mapped: std::collections::BTreeSet<&str> = cols.iter().map(|c| eval_domain_of(c).unwrap()).collect();
        assert_eq!(mapped.len(), 18);
        for d in EVAL_DOMAINS {
            assert!(mapped.contains(d) && eval_register(d).is_some(), "{d}");
        }
        for c in ["f1000research_reports_historical", "icsi_meeting_corpus_transcripts", "stackexchange_historical", "plos_blog_historical",
            "amazon_reviews_2023_historical", "govinfo_congressional_hearings_historical"]
        {
            assert_eq!(eval_domain_of(c), None, "{c}");
        }
        // the original seven: evaluation mapping and registers agree with the Train-side ones
        for c in &cols[..7] {
            let d = eval_domain_of(c).unwrap();
            assert_eq!(domain_of(c).unwrap(), d);
            assert_eq!(register(d), eval_register(d).unwrap());
            assert_eq!(split_register("cal", d), register(d));
        }
        assert_eq!(split_register("train", "student_essays"), "a student essay");
        assert_eq!(split_register("test", "student_essays"), "an argumentative essay by a secondary-school student");
    }
    fn human(split: &str, domain: &'static str, i: usize) -> Human {
        let text = format!(
            "Sentence one about item {i} goes here. The second sentence adds more words to the passage. A third sentence keeps \
             going with detail. Then a fourth one appears. The fifth sentence is somewhat longer than the others in this text. \
             Sixth comes next. Finally the seventh sentence closes the excerpt for document {i}."
        );
        Human {
            split: split.into(),
            domain,
            id: format!("{domain}-doc-{i}"),
            text_sha256: text::sha_hex(text.as_bytes()),
            word_count: text::words(&text),
            text,
            title: if i % 3 == 0 { None } else { Some(format!("Title {i}")) },
            source_group: format!("g{}", i / 2),
            genre: "g".into(),
        }
    }
    #[test]
    fn build_test_follows_the_test_plan() {
        let mut hs: Vec<Human> = (0..45).map(|i| human("test", "spoken_transcripts", i)).collect();
        hs.extend((0..22).map(|i| human("test", "wikipedia", i)));
        let idf: HashMap<&'static str, Idf> = EVAL_DOMAINS.iter().map(|d| (*d, Idf::default())).collect();
        let p = build_test(&hs, &idf).unwrap();
        for (d, n) in [("spoken_transcripts", 45usize), ("wikipedia", 22)] {
            let (olmo, robust, _) = test_quota(n);
            let c = |cohort: &str, g: &str| p.items.iter().filter(|i| i.domain == d && i.cohort == cohort && i.generator == g).count();
            assert_eq!(c("primary_mirror", TEST_ONLY_INSTRUCT), olmo);
            let fails = p.failures.iter().filter(|f| f["generator"] == TEST_ONLY_INSTRUCT && f["cohort"] == "primary_mixed").count();
            assert_eq!(c("primary_mixed", TEST_ONLY_INSTRUCT) + fails, olmo);
            let rf = p.failures.iter().filter(|f| f["cohort"] == TEST_ROBUSTNESS_COHORT).count();
            assert_eq!(rf, 0);
            assert_eq!(c(TEST_ROBUSTNESS_COHORT, TEST_ONLY_BASE), robust);
            assert_eq!(p.items.iter().filter(|i| i.domain == d && i.cohort == "primary_mirror").count(), n);
        }
        let rr = ranks(&hs, "robust");
        let by_id: HashMap<&str, &Human> = hs.iter().map(|h| (h.id.as_str(), h)).collect();
        for it in p.items.iter().filter(|i| i.cohort == TEST_ROBUSTNESS_COHORT) {
            let r = rr[&it.parent_id];
            assert_eq!(r % 2, 0);
            assert_eq!(it.kind, if (r / 2) % 2 == 0 { "continuation" } else { "splice" });
            assert_eq!(it.ranks["robust"], r);
            assert_eq!((it.decoding.temperature, it.decoding.top_p, it.decoding.max_tokens), (0.8, 0.95, 1024));
            assert_eq!((it.decoding.top_k, it.decoding.min_p, it.decoding.repeat_penalty), (0, 0.0, 1.0));
            let req = p.requests.iter().find(|q| q.item_id == it.item_id).unwrap();
            let h = by_id[it.parent_id.as_str()];
            let (s, e) = it.source_spans[0];
            assert_eq!(req.endpoint, "completion");
            assert!(h.text.starts_with(&req.prompt) && req.prompt.len() <= s);
            assert_eq!(h.text[req.prompt.len()..s].trim(), "");
            let pw = text::words(&req.prompt);
            let need = if it.kind == "continuation" { 30 } else { 25 };
            assert!(pw * 100 >= need * h.word_count);
            if it.kind == "continuation" {
                assert_eq!(e, h.text.len());
            } else {
                assert!(text::words(&h.text[e..]) * 100 >= 25 * h.word_count && e < h.text.len());
            }
            assert_eq!(req.cut_budget, Some(text::words(&h.text[s..e])));
            assert_eq!(req.target_words, text::words(&h.text[s..e]));
            assert_eq!(req.seed, seed_of(&req.request_sha256));
        }
        for it in p.items.iter().filter(|i| i.cohort == "primary_mirror") {
            let req = p.requests.iter().find(|q| q.item_id == it.item_id).unwrap();
            assert!(req.prompt.starts_with(&format!("Write {} in English", eval_register(&it.domain).unwrap())));
            assert!(["A", "B"].contains(&it.decoding.profile.as_str()));
        }
        // keys are the protocol v1 keys within (test, domain)
        let rm = ranks(&hs, "mirror_gen");
        for it in p.items.iter().filter(|i| i.cohort == "primary_mirror") {
            assert_eq!(it.generator, eval_generator("test", rm[&it.parent_id] % 21));
        }
        assert!(build_test(&[human("cal", "wikipedia", 1)], &idf).is_err());
    }
    #[test]
    fn base_runs_prefix_and_suffix_rules() {
        let c = cum(&[10; 10]);
        assert_eq!(base_runs(&c, true), Some((3, 10)));
        assert_eq!(base_runs(&c, false), Some((3, 7)));
        assert_eq!(base_runs(&cum(&[100]), true), None);
        assert_eq!(base_runs(&cum(&[30, 40, 30]), false), Some((1, 2)));
        assert_eq!(base_runs(&cum(&[50, 50]), false), None);
    }
    #[test]
    fn enron_rows_are_a_domain_of_the_train_only_planner_only() {
        let t = "Synthetic words of one message.";
        let row = json!({"id": "e1", "frozen_split": "train", "origin": "human_only", "text": t, "text_sha256": text::sha_hex(t.as_bytes()),
            "word_count": text::words(t), "collection": grammar_a_pools::pile::ENRON_COLLECTION, "provenance": {}, "source_group": "g", "domain": "email"});
        // every shared Train reader refuses it as before (style, brief, library and plan readers)
        assert!(domain_of(grammar_a_pools::pile::ENRON_COLLECTION).is_err());
        assert_eq!(human_from(&row, "train").err().unwrap().to_string(), format!("unadmitted collection {}", grammar_a_pools::pile::ENRON_COLLECTION));
        // the Train-only planner's reader maps it, and maps every other collection as domain_of does
        assert_eq!(human_from_in(&row, "train", true).unwrap().domain, ENRON_DOMAIN);
        assert_eq!(train_only_domain_of("wikipedia_enwiki_historical").unwrap(), "wikipedia");
        assert!(train_only_domain_of("mystery_archive_historical").is_err());
        assert!(!DOMAINS.contains(&ENRON_DOMAIN));
    }
}
