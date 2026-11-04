//! Serverless challenge text generation (Workers AI), as pure functions.
//!
//! - **mixed**: a contiguous run of sentences of the batch's own human parent is
//!   rewritten by a model and spliced back (`sentence_splice_v1`). The exact
//!   final byte span of the model text is recorded, and share is the realized
//!   final-word fraction, `words(replacement) / words(final)` under
//!   `unicode_split_whitespace_v1`. At least one human sentence always remains.
//! - **model_only**: written from scratch (`register_topic_scratch_v1`) from a
//!   domain register and a topic chosen by private digest, with no human text in
//!   the prompt. Source-conditioned rewrites and scratch writing stay separate.
//!
//! Outputs that echo instructions, add preambles or markup, think aloud, leave
//! the text unchanged or miss the length window are rejected and retried on the
//! next generator, as is a final document over the scorer's token limit (the
//! cohort's check, `v3::scorer_length`). Nothing here logs or returns text
//! outside the batch.

use serde::{Deserialize, Serialize};

pub const SPLICE_POLICY: &str = "sentence_splice_v1";
pub const SCRATCH_POLICY: &str = "register_topic_scratch_v1";
pub const WORD_POLICY: &str = "unicode_split_whitespace_v1";
pub const PROMPT_VERSION: &str = "testnet_generation_prompts_v1";
pub const TEMPERATURE: f64 = 0.7;
pub const TOP_P: f64 = 0.9;

pub fn words(s: &str) -> usize {
    s.split_whitespace().count()
}

/// Workers AI list prices (developers.cloudflare.com/workers-ai/platform/pricing,
/// read 2026-10-30), in micro-dollars per million input and output tokens.
const PRICES: &[(&str, u64, u64)] = &[
    ("@cf/meta/llama-3.2-1b-instruct", 27_000, 201_000),
    ("@cf/meta/llama-3.2-3b-instruct", 51_000, 335_000),
    ("@cf/meta/llama-3.1-8b-instruct-fp8-fast", 45_000, 384_000),
    ("@cf/mistral/mistral-7b-instruct-v0.1", 110_000, 190_000),
    ("@cf/mistralai/mistral-small-3.1-24b-instruct", 351_000, 555_000),
    ("@cf/google/gemma-3-12b-it", 345_000, 556_000),
    ("@cf/google/gemma-4-26b-a4b-it", 100_000, 300_000),
    ("@cf/ibm-granite/granite-4.0-h-micro", 17_000, 112_000),
    ("@cf/qwen/qwen3-30b-a3b-fp8", 51_000, 335_000),
    ("@cf/zai-org/glm-4.7-flash", 60_000, 400_000),
    ("@cf/meta/llama-4-scout-17b-16e-instruct", 270_000, 850_000),
];

pub fn price(model: &str) -> Option<(u64, u64)> {
    PRICES
        .iter()
        .find(|(m, _, _)| *m == model)
        .map(|(_, i, o)| (*i, *o))
}

/// Cost in micro-dollars, rounded up.
pub fn cost_microusd(model: &str, prompt_tokens: u64, completion_tokens: u64) -> u64 {
    let (pi, po) = price(model).unwrap_or((1_000_000, 1_000_000));
    (prompt_tokens * pi + completion_tokens * po).div_ceil(1_000_000)
}

/// Domain registers. A domain absent here uses the generic register.
const REGISTERS: &[(&str, &str)] = &[
    ("wikipedia", "the lead section of an English Wikipedia article, in a neutral encyclopedic register"),
    ("project_gutenberg", "a passage of narrative prose in the style of a nineteenth-century English novel"),
    ("plos", "the introduction of a peer-reviewed open-access research article"),
    ("fineweb", "a personal blog post written in the first person"),
    ("global_voices", "a citizen-media news article in the style of Global Voices"),
    ("us_federal_register", "a notice published in the United States Federal Register"),
    ("wikinews", "a short news report in the style of Wikinews"),
];

pub fn register(domain: &str) -> &'static str {
    REGISTERS
        .iter()
        .find(|(d, _)| *d == domain)
        .map(|(_, r)| *r)
        .unwrap_or("a short piece of English prose")
}

const TOPICS: &[(&str, &[&str])] = &[
    ("wikipedia", &[
        "a small river in northern England", "a nineteenth-century botanist", "a medieval castle in Wales",
        "a species of freshwater snail", "a regional railway line", "a jazz saxophonist from the 1950s",
        "a mountain pass in the Andes", "an early computer programming language", "a coastal town in Portugal",
        "a minor planet discovered in 1930", "a traditional Japanese pottery style", "a defunct newspaper",
        "a Baroque church in Bavaria", "a football club in the Scottish lower leagues", "a desert lizard",
        "an annual folk music festival",
    ]),
    ("project_gutenberg", &[
        "a sea voyage that meets a storm", "an inheritance dispute in a country house", "a walk across moorland at dusk",
        "a reunion between estranged brothers", "a village fair and its gossip", "a clerk's first day in London",
        "a letter that arrives too late", "a winter journey by coach", "a governess and a difficult child",
        "a ruined abbey visited at night", "a quarrel over a debt", "a ball where a secret is overheard",
        "a fisherman's family awaiting news", "a doctor called to a remote farm", "an old soldier's memories",
        "a garden in late summer",
    ]),
    ("plos", &[
        "soil salinity and rice yield", "antibiotic resistance in hospital wastewater", "sleep duration in adolescents",
        "coral bleaching and water temperature", "gut microbiota in elderly patients", "bat foraging near wind turbines",
        "air pollution and childhood asthma", "pollinator decline in farmland", "malaria vector behaviour",
        "protein folding in heat-stressed yeast", "maternal nutrition and birth weight", "invasive crayfish in rivers",
        "physical activity after stroke", "drought tolerance in wheat", "vaccine hesitancy in rural communities",
        "noise exposure and hearing loss",
    ]),
    ("fineweb", &[
        "upgrading a home office internet connection", "learning to bake sourdough bread", "a weekend camping trip",
        "switching to a standing desk", "restoring an old bicycle", "starting a vegetable garden on a balcony",
        "a frustrating experience with customer support", "teaching a dog new tricks", "moving to a new city",
        "choosing a first digital camera", "running a first half marathon", "fixing a leaking kitchen tap",
        "keeping a reading journal", "a local farmers market", "learning to play the ukulele",
        "organizing a cluttered garage",
    ]),
    ("global_voices", &[
        "protests over rising bread prices", "a community radio station in a rural district", "internet shutdowns during an election",
        "a women's cooperative selling handicrafts", "flooding in a coastal capital", "a debate over a new language law",
        "young bloggers covering local corruption", "a festival revived after decades", "migrant workers sending money home",
        "a campaign to save a historic market", "teachers striking over unpaid wages", "a viral video about police conduct",
        "farmers adapting to drought", "a new metro line dividing opinion", "journalists facing travel bans",
        "a grassroots recycling project",
    ]),
    ("us_federal_register", &[
        "a proposed rule on commercial fishing permits", "a public meeting of an advisory committee",
        "an information collection request for a grant program", "an airworthiness directive for a helicopter model",
        "a notice of intent to prepare an environmental impact statement", "changes to a wildlife refuge hunting plan",
        "a request for comments on broadband mapping", "a final rule on pesticide tolerances",
        "a notice of a meeting of a fisheries council", "an extension of a comment period",
        "a determination on a trade petition", "a revision of a privacy act system of records",
        "an emergency closure of a fishery", "the availability of a draft recovery plan",
        "a petition to list a plant species", "a change in reporting requirements for lenders",
    ]),
    ("wikinews", &[
        "a regional election result", "a bridge closure after an inspection", "a new museum opening",
        "a cycling race won in a sprint finish", "a factory fire with no injuries", "a court ruling on a zoning dispute",
        "a university research grant", "a storm causing power cuts", "a city council budget vote",
        "a rare bird sighting", "a transport strike", "a charity fundraiser record",
        "a space launch delay", "a school building reopened", "a food safety recall", "a mountain rescue operation",
    ]),
];

pub fn topic(domain: &str, digest: &[u8; 32]) -> &'static str {
    let list = TOPICS
        .iter()
        .find(|(d, _)| *d == domain)
        .map(|(_, t)| *t)
        .unwrap_or(TOPICS[0].1);
    list[usize::from(digest[2]) % list.len()]
}

/// Target length for a scratch document, in words.
pub fn scratch_words(digest: &[u8; 32]) -> usize {
    [150, 200, 250, 300][usize::from(digest[3]) % 4]
}

/// Sentence byte ranges of `text`: `[start, end)` with trailing whitespace excluded.
/// A sentence ends after `.`, `!` or `?` (and any closing quotes or brackets)
/// followed by whitespace, or at a line break.
pub fn sentences(text: &str) -> Vec<(usize, usize)> {
    let b = text.as_bytes();
    let mut out = Vec::new();
    let mut start = None;
    let mut i = 0;
    let push = |out: &mut Vec<(usize, usize)>, s: usize, e: usize| {
        let mut e = e;
        while e > s && b[e - 1].is_ascii_whitespace() {
            e -= 1;
        }
        if e > s {
            out.push((s, e));
        }
    };
    while i < b.len() {
        let c = b[i];
        if start.is_none() {
            if !c.is_ascii_whitespace() {
                start = Some(i);
            }
            i += 1;
            continue;
        }
        if c == b'\n' {
            push(&mut out, start.unwrap(), i);
            start = None;
            i += 1;
            continue;
        }
        if matches!(c, b'.' | b'!' | b'?') {
            let mut j = i + 1;
            while j < b.len() && matches!(b[j], b'"' | b'\'' | b')' | b']') {
                j += 1;
            }
            // Multi-byte closing quotes (U+201D, U+2019).
            while j + 3 <= b.len()
                && (&b[j..j + 3] == "\u{201d}".as_bytes() || &b[j..j + 3] == "\u{2019}".as_bytes())
            {
                j += 3;
            }
            if j >= b.len() || b[j].is_ascii_whitespace() {
                push(&mut out, start.unwrap(), j);
                start = None;
                i = j;
                continue;
            }
        }
        i += 1;
    }
    if let Some(s) = start {
        push(&mut out, s, b.len());
    }
    out
}

/// The sentences `[first, last]` to rewrite: 10, 40 or 80 % of the sentences at the
/// start, middle or end, chosen by digest, always leaving one human sentence.
pub fn select_span(count: usize, digest: &[u8; 32]) -> Option<(usize, usize, u32)> {
    if count < 2 {
        return None;
    }
    let pct = [10u32, 40, 80][usize::from(digest[0]) % 3];
    let k = ((count as u64 * u64::from(pct) + 50) / 100).clamp(1, count as u64 - 1) as usize;
    let first = match digest[1] % 3 {
        0 => 0,
        1 => (count - k) / 2,
        _ => count - k,
    };
    Some((first, first + k - 1, pct))
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Prompt {
    pub system: String,
    pub user: String,
    pub max_tokens: u32,
}

pub fn rewrite_prompt(domain: &str, passage: &str) -> Prompt {
    let n = words(passage);
    Prompt {
        system: "You are a careful copy editor. Rewrite the passage the user gives you in your own words. \
Keep its meaning, facts, names, numbers, perspective and register, and keep about the same length. \
Reply with the rewritten passage only: no preamble, no notes, no quotation marks, no headings or formatting."
            .into(),
        user: format!(
            "The passage comes from {}. Rewrite it in about {n} words.\n\n{passage}",
            register(domain)
        ),
        max_tokens: (n as u32 * 3).clamp(96, 1024),
    }
}

/// The versioned set of rewrite voices and operations (`reward-v1` mixed
/// children). Each use of a rewrite source takes a voice and an operation by
/// the item's private digest, avoiding those of the source's earlier uses
/// (decision 28: a fresh generator, seed, voice and operation each time).
/// Every operation keeps the passage's meaning and about its length, so the
/// output passes [`validate_rewrite`] like the plain prompt. With at least as
/// many voices and operations as the plan's reuse limit, no source repeats
/// one. A change to either list is a new set identifier.
pub const REWRITE_PROMPT_SET: &str = "rewrite_voice_operation_v1";

pub const REWRITE_VOICES: &[(&str, &str)] = &[
    ("copy_editor", "You are a careful copy editor."),
    ("news_subeditor", "You are a newspaper sub-editor who prefers plain, direct sentences."),
    ("academic_editor", "You are an academic editor who prefers precise, measured wording."),
    ("plain_language", "You are a plain-language editor who favours short common words."),
    ("magazine_writer", "You are a magazine writer with an easy, readable style."),
    ("technical_writer", "You are a technical writer who values clarity and exact terms."),
];

pub const REWRITE_OPERATIONS: &[(&str, &str)] = &[
    ("paraphrase", "Rewrite the passage in your own words."),
    ("restructure", "Rewrite the passage by restructuring its sentences: combine, split or reorder clauses."),
    ("vocabulary", "Rewrite the passage with fresh word choices and phrasing throughout."),
    ("voice_shift", "Rewrite the passage, changing active and passive constructions where natural."),
    ("smooth", "Rewrite the passage so that it reads more smoothly, with better transitions."),
    ("tighten", "Rewrite the passage more crisply, removing redundancy without dropping any content."),
];

/// The voice and operation of a rewrite with private item digest `digest`,
/// avoiding `avoid_voices` and `avoid_operations` where the set offers
/// another (the source's earlier uses).
pub fn rewrite_variant(digest: &[u8; 32], avoid_voices: &[String], avoid_operations: &[String]) -> (&'static str, &'static str) {
    fn pick(list: &'static [(&'static str, &'static str)], start: u8, avoid: &[String]) -> &'static str {
        let n = list.len();
        (0..n)
            .map(|k| list[(usize::from(start) + k) % n].0)
            .find(|id| !avoid.iter().any(|a| a == id))
            .unwrap_or(list[usize::from(start) % n].0)
    }
    (pick(REWRITE_VOICES, digest[5], avoid_voices), pick(REWRITE_OPERATIONS, digest[6], avoid_operations))
}

/// The rewrite prompt for a voice and an operation of [`REWRITE_PROMPT_SET`].
/// Unknown identifiers fall back to the plain prompt's voice and operation.
pub fn rewrite_prompt_variant(domain: &str, passage: &str, voice: &str, operation: &str) -> Prompt {
    let n = words(passage);
    let voice = REWRITE_VOICES.iter().find(|(id, _)| *id == voice).map_or(REWRITE_VOICES[0].1, |(_, v)| *v);
    let op = REWRITE_OPERATIONS.iter().find(|(id, _)| *id == operation).map_or(REWRITE_OPERATIONS[0].1, |(_, v)| *v);
    Prompt {
        system: format!(
            "{voice} {op} Keep its meaning, facts, names, numbers, perspective and register, and keep about the \
same length. Reply with the rewritten passage only: no preamble, no notes, no quotation marks, no headings or \
formatting."
        ),
        user: format!(
            "The passage comes from {}. Rewrite it in about {n} words.\n\n{passage}",
            register(domain)
        ),
        max_tokens: (n as u32 * 3).clamp(96, 1024),
    }
}

pub fn scratch_prompt(domain: &str, topic: &str, target_words: usize) -> Prompt {
    Prompt {
        system: "You are a writer. Write the requested text as plain prose paragraphs. \
Reply with the text only: no title, no preamble, no notes, no headings, lists or formatting."
            .into(),
        user: format!(
            "Write {} about {topic}, about {target_words} words long.",
            register(domain)
        ),
        max_tokens: (target_words as u32 * 3).clamp(256, 1200),
    }
}

/// Why an output was refused (content-free reason codes).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Reject {
    Empty,
    Preamble,
    Markup,
    Thinking,
    Unchanged,
    /// Fewer words than the length window allows (rewrite or scratch).
    Short,
    /// More words than the length window allows (rewrite or scratch).
    Long,
    /// The final document (a spliced mixed child, or a scratch text) is over
    /// the scorer's token limit under the pinned tokenizer, so no artifact
    /// could score it (`v3::scorer_length`). Set by the cohort, never by
    /// a lane.
    TokenLimit,
    Transport,
    Budget,
    /// Workers AI rate limit or capacity (HTTP 429, code 3040): no attempt used.
    Throttled,
    /// The provider no longer serves the model (codes 5007, 3042): the attempt
    /// is used and the generator rotates.
    ModelUnavailable,
}

impl Reject {
    pub fn code(self) -> &'static str {
        match self {
            Self::Empty => "empty",
            Self::Preamble => "preamble",
            Self::Markup => "markup",
            Self::Thinking => "thinking",
            Self::Unchanged => "unchanged",
            // The direction is recorded so the window can be checked against
            // measured rejections (before 2026-11-03 both were `length`).
            Self::Short => "length_short",
            Self::Long => "length_long",
            Self::TokenLimit => "length_tokens",
            Self::Transport => "transport",
            Self::Budget => "budget",
            Self::Throttled => "throttled",
            Self::ModelUnavailable => "model_unavailable",
        }
    }
}

/// Classifies a Workers AI binding error from its text: the Rust binding
/// exposes neither the HTTP status nor the error code, so the text is all
/// there is. The errors page documents 3040 (capacity, 429), 3036 (daily free
/// allocation, 429), 5007 and 3042 (no such model); no separate code is
/// documented for the requests-a-minute limit, so any 429 or rate-limit
/// wording is a throttle.
pub fn classify_ai_error(text: &str) -> Reject {
    let t = text.to_ascii_lowercase();
    if ["3040", "3036", "429", "capacity temporarily exceeded", "rate limit", "too many requests", "rate-limit"]
        .iter()
        .any(|k| t.contains(k))
    {
        return Reject::Throttled;
    }
    if ["5007", "3042", "no such model", "invalid model"].iter().any(|k| t.contains(k)) {
        return Reject::ModelUnavailable;
    }
    Reject::Transport
}

/// Openings that address the requester instead of being the text. Kept to
/// phrases, so first-person prose ("I've always...") is not refused.
const PREAMBLES: &[&str] = &[
    "here is the", "here's the", "here is a", "here's a", "here is my", "here's my", "sure,", "sure!",
    "sure.", "certainly,", "certainly!", "certainly.", "okay,", "of course,", "of course!", "rewritten",
    "the rewritten", "rewrite:", "i have rewritten", "i've rewritten", "as requested", "below is",
    "revised version", "revised passage", "paraphrased", "title:", "note:",
];

fn common_checks(out: &str) -> Result<String, Reject> {
    let mut t = out.trim().to_string();
    if t.contains("<think") || t.contains("</think>") {
        return Err(Reject::Thinking);
    }
    // A whole-text quotation is unwrapped once.
    if t.len() >= 2 && ((t.starts_with('"') && t.ends_with('"')) || (t.starts_with('\u{201c}') && t.ends_with('\u{201d}'))) {
        let inner_start = t.chars().next().map_or(0, char::len_utf8);
        let inner_end = t.len() - t.chars().last().map_or(0, char::len_utf8);
        t = t[inner_start..inner_end].trim().to_string();
    }
    if t.is_empty() {
        return Err(Reject::Empty);
    }
    let lower = t.to_lowercase();
    if PREAMBLES.iter().any(|p| lower.starts_with(p)) {
        return Err(Reject::Preamble);
    }
    if t.contains("**") || t.contains("```") || t.lines().any(|l| {
        let l = l.trim_start();
        l.starts_with('#') || l.starts_with("- ") || l.starts_with("* ")
    }) {
        return Err(Reject::Markup);
    }
    Ok(t)
}

/// Checks a rewrite of `source`; returns the replacement text to splice.
pub fn validate_rewrite(source: &str, out: &str) -> Result<String, Reject> {
    let t = common_checks(out)?;
    let norm = |s: &str| s.split_whitespace().collect::<Vec<_>>().join(" ");
    if norm(&t) == norm(source) {
        return Err(Reject::Unchanged);
    }
    let (a, b) = (words(source) as f64, words(&t) as f64);
    if b < 0.5 * a {
        return Err(Reject::Short);
    }
    if b > 1.8 * a + 4.0 {
        return Err(Reject::Long);
    }
    // A single-paragraph source keeps a single paragraph.
    if !source.contains('\n') && t.contains('\n') {
        return Ok(t.split_whitespace().collect::<Vec<_>>().join(" "));
    }
    Ok(t)
}

pub fn validate_scratch(out: &str, target_words: usize) -> Result<String, Reject> {
    let t = common_checks(out)?;
    let n = words(&t) as f64;
    let target = target_words as f64;
    if n < (0.6 * target).max(80.0) {
        return Err(Reject::Short);
    }
    if n > 1.8 * target {
        return Err(Reject::Long);
    }
    Ok(t)
}

/// A spliced mixed document.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Spliced {
    pub text: String,
    /// Final byte span of the model text.
    pub span: (usize, usize),
    pub words_total: usize,
    pub words_ai: usize,
}

impl Spliced {
    /// Realized share in parts per billion, rounded half up.
    pub fn share_ppb(&self) -> u32 {
        ((self.words_ai as u64 * 2_000_000_000 + self.words_total as u64) / (2 * self.words_total as u64)) as u32
    }
}

pub fn splice(parent: &str, range: (usize, usize), replacement: &str) -> Spliced {
    let mut text = String::with_capacity(parent.len() + replacement.len());
    text.push_str(&parent[..range.0]);
    text.push_str(replacement);
    text.push_str(&parent[range.1..]);
    let span = (range.0, range.0 + replacement.len());
    Spliced {
        words_total: words(&text),
        words_ai: words(replacement),
        span,
        text,
    }
}

/// Generator for an item's attempt: rotation by private digest, then the next one.
/// The rotation's generator for `attempt`, skipping those in `avoid` (a
/// reused parent's earlier children) while another remains.
pub fn generator_avoiding<'a>(generators: &'a [String], digest: &[u8; 32], attempt: u32, avoid: &[String]) -> &'a str {
    let n = generators.len().max(1) as u32;
    for k in 0..n {
        let g = generator(generators, digest, attempt + k);
        if !avoid.iter().any(|a| a == g) {
            return g;
        }
    }
    generator(generators, digest, attempt)
}

pub fn generator<'a>(generators: &'a [String], digest: &[u8; 32], attempt: u32) -> &'a str {
    let i = (usize::from(digest[4]) + attempt as usize) % generators.len();
    &generators[i]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rewrite_variants_avoid_earlier_uses_and_keep_the_prompt_shape() {
        let d = [7u8; 32];
        let (v, o) = rewrite_variant(&d, &[], &[]);
        let all_v: Vec<String> = REWRITE_VOICES.iter().map(|(id, _)| (*id).to_owned()).collect();
        let all_o: Vec<String> = REWRITE_OPERATIONS.iter().map(|(id, _)| (*id).to_owned()).collect();
        // Avoiding all but one leaves exactly that one.
        for keep in 0..REWRITE_VOICES.len() {
            let mut avoid_v = all_v.clone();
            let kept_v = avoid_v.remove(keep);
            let mut avoid_o = all_o.clone();
            let kept_o = avoid_o.remove(keep % REWRITE_OPERATIONS.len());
            assert_eq!(rewrite_variant(&d, &avoid_v, &avoid_o), (kept_v.as_str(), kept_o.as_str()));
        }
        // Avoiding everything falls back to the digest's own choice.
        assert_eq!(rewrite_variant(&d, &all_v, &all_o), (v, o));
        // Four uses (the reuse limit) never repeat a voice or an operation.
        let (mut vs, mut os) = (Vec::new(), Vec::new());
        for k in 0..4u8 {
            let (v, o) = rewrite_variant(&[k; 32], &vs, &os);
            assert!(!vs.contains(&v.to_owned()) && !os.contains(&o.to_owned()));
            vs.push(v.to_owned());
            os.push(o.to_owned());
        }
        let p = rewrite_prompt_variant("wikipedia", "One. Two three.", v, o);
        assert!(p.system.contains("Keep its meaning") && p.user.ends_with("One. Two three."));
        assert!(p.system.starts_with(REWRITE_VOICES.iter().find(|(id, _)| *id == v).unwrap().1));
    }

    #[test]
    fn sentences_split_on_terminal_punctuation_and_lines() {
        let t = "First one. Second \"quoted.\" Third?\nFourth line without stop\n\n  Fifth! ";
        let s: Vec<&str> = sentences(t).into_iter().map(|(a, b)| &t[a..b]).collect();
        assert_eq!(s, ["First one.", "Second \"quoted.\"", "Third?", "Fourth line without stop", "Fifth!"]);
        assert!(sentences("   ").is_empty());
        assert_eq!(sentences("No terminal punctuation").len(), 1);
        let u = "He said \u{201c}yes.\u{201d} Then left.";
        assert_eq!(sentences(u).len(), 2);
    }

    #[test]
    fn spans_always_leave_a_human_sentence() {
        for n in 2..40 {
            for d0 in 0..3u8 {
                for d1 in 0..3u8 {
                    let mut d = [0u8; 32];
                    d[0] = d0;
                    d[1] = d1;
                    let (a, b, _) = select_span(n, &d).unwrap();
                    assert!(a <= b && b < n && (b - a + 1) < n, "n {n}");
                }
            }
        }
        assert!(select_span(1, &[0; 32]).is_none());
    }

    #[test]
    fn splice_records_exact_span_and_word_share() {
        let parent = "Alpha beta gamma. Delta epsilon. Zeta eta theta iota.";
        let s = sentences(parent);
        let r = (s[1].0, s[1].1);
        let out = splice(parent, r, "Rewritten words here now.");
        assert_eq!(&out.text[out.span.0..out.span.1], "Rewritten words here now.");
        assert_eq!(out.text, "Alpha beta gamma. Rewritten words here now. Zeta eta theta iota.");
        assert_eq!((out.words_ai, out.words_total), (4, 11));
        assert_eq!(out.share_ppb(), 363_636_364);
    }

    #[test]
    fn output_validation_rejects_echoes_and_markup() {
        let src = "The council approved the plan on Monday after a long debate about costs.";
        assert_eq!(validate_rewrite(src, src), Err(Reject::Unchanged));
        assert_eq!(validate_rewrite(src, "Here is the rewrite: The council..."), Err(Reject::Preamble));
        assert_eq!(validate_rewrite(src, "Sure, the council approved the plan on Monday after debate on costs."), Err(Reject::Preamble));
        // First-person prose is not a preamble.
        let blog = format!("I've always liked old bicycles. {}", "word ".repeat(150));
        assert!(validate_scratch(&blog, 150).is_ok());
        assert_eq!(validate_rewrite(src, "**The council** approved it."), Err(Reject::Markup));
        assert_eq!(validate_rewrite(src, "<think>hmm</think> ok"), Err(Reject::Thinking));
        assert_eq!(validate_rewrite(src, "Approved."), Err(Reject::Short));
        let padded = format!("{src} {src} and then some more words about it");
        assert_eq!(validate_rewrite(src, &padded), Err(Reject::Long));
        assert_eq!((Reject::Short.code(), Reject::Long.code()), ("length_short", "length_long"));
        assert_eq!(Reject::TokenLimit.code(), "length_tokens");
        let ok = "On Monday, after lengthy debate over costs, the council gave the plan its approval.";
        assert_eq!(validate_rewrite(src, &format!("\"{ok}\"")).unwrap(), ok);
        let long = "word ".repeat(300);
        assert_eq!(validate_scratch(&long, 150).unwrap_err(), Reject::Long);
        assert_eq!(validate_scratch(&"word ".repeat(60), 150).unwrap_err(), Reject::Short);
        assert!(validate_scratch(&"word ".repeat(150), 150).is_ok());
    }

    #[test]
    fn prices_and_costs() {
        assert_eq!(cost_microusd("@cf/meta/llama-3.2-3b-instruct", 1_000_000, 1_000_000), 386_000);
        assert_eq!(cost_microusd("@cf/ibm-granite/granite-4.0-h-micro", 400, 300), 41);
        assert!(price("@cf/none").is_none());
        let gens: Vec<String> = vec!["a".into(), "b".into(), "c".into()];
        let mut d = [0u8; 32];
        d[4] = 4;
        assert_eq!(generator(&gens, &d, 0), "b");
        assert_eq!(generator(&gens, &d, 1), "c");
    }
}
