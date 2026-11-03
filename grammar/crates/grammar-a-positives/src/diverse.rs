//! Plan v2: style- and task-diverse positive cohorts.
//!
//! Evaluation splits keep the protocol-v1.1 `positive_plan` primary cohorts
//! unchanged (built by `plan::build`, identical item and request ids) and gain
//! a separately labelled supplementary diverse cohort. Train replaces the
//! fixed-template primary cohorts with diverse mirrors and rewrites.
//!
//! Mirrors vary the document type (native or one of many off-domain types),
//! length (matched to the human), style and decoding. Rewrites vary the
//! operation (11 kinds, weighted), requested share bin (7 bins including
//! (0,10)), style and decoding. Style comes from library v2: about 25% of
//! style-capable items carry no style instruction; the rest use a short
//! "sound like ..." voice prompt (alone, with a modifier, or in a persona
//! combination with an era/place and a mood) or a free-form genre brief.
//! Base checkpoints produce continuations and splices only. Factors are
//! assigned by independent keyed ranks within (split, domain); library items
//! are used round-robin within their split partition. No detector score is
//! used anywhere.
use crate::plan::{self, Decoding, Human, Item, Plan, Request};
use crate::text::{self, Idf};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap};

pub const V2_KEY_PREFIX: &str = "slopninja-a-comp-plan-v2:";
pub const PLAN_V2_SCHEMA: &str = "slop_ninja_a_comp_positive_plan_v2";
pub const STYLE_POLICY: &str = "a-comp-plan-v2-style-v2";

/// Operation slots (weighted by repetition): 17 slots.
pub const OP_SLOTS: [&str; 17] = [
    "paraphrase", "paraphrase", "paraphrase", "expand", "expand", "condense", "condense", "continue", "continue", "humanize", "humanize",
    "casual", "simplify", "polish", "formalize", "fix_slop", "spelling",
];
pub const OPS: [&str; 11] = [
    "paraphrase", "polish", "expand", "condense", "formalize", "casual", "simplify", "continue", "humanize", "fix_slop", "spelling",
];
/// Operations that may carry a voice or brief (the others define their own register).
pub const BRIEF_OPS: [&str; 7] = ["paraphrase", "expand", "condense", "continue", "casual", "humanize", "simplify"];
pub const BINS: [&str; 7] = ["R0", "R1", "R2", "R3", "R4", "R5", "R6"];

/// Off-domain mirror document types: (id, prompt phrase, brief genre).
pub const OFF_TYPES: [(&str, &str, &str); 36] = [
    ("personal_email", "a personal email", "personal_web"),
    ("forum_post", "a forum post", "personal_web"),
    ("product_review", "a product review", "personal_web"),
    ("book_review", "a book review", "essays_ideas"),
    ("blog_post", "a blog post", "personal_web"),
    ("news_report", "a news report", "news"),
    ("encyclopedia_entry", "an encyclopedia entry", "encyclopedia"),
    ("student_essay", "a student essay", "essays_ideas"),
    ("opinion_column", "an opinion column", "essays_ideas"),
    ("cover_letter", "a cover letter", "personal_web"),
    ("technical_explanation", "a technical explanation", "science_nature"),
    ("short_story", "a short story", "fiction"),
    ("speech", "a speech", "personal_web"),
    ("faq_answer", "an answer to a frequently asked question", "personal_web"),
    ("lab_report_summary", "a lab report summary", "science"),
    ("letter_to_editor", "a letter to the editor", "essays_ideas"),
    ("diary_entry", "a diary entry", "personal_web"),
    ("press_release", "a press release", "news"),
    ("travel_guide", "a travel guide section", "travel"),
    ("restaurant_review", "a restaurant review", "personal_web"),
    ("newsletter", "a community newsletter item", "news"),
    ("podcast_script", "a podcast script", "personal_web"),
    ("social_thread", "a social media thread", "personal_web"),
    ("complaint_letter", "a complaint letter to a company", "personal_web"),
    ("how_to_guide", "a how-to guide", "science_nature"),
    ("museum_label", "a museum exhibit text", "history_biography"),
    ("obituary", "an obituary", "history_biography"),
    ("grant_summary", "a grant proposal summary", "science"),
    ("meeting_minutes", "meeting minutes", "government"),
    ("public_notice", "a public notice", "government"),
    ("textbook_section", "a textbook section", "encyclopedia"),
    ("wedding_toast", "a wedding toast", "personal_web"),
    ("advice_column", "an advice column answer", "personal_web"),
    ("interview_answer", "an answer in a magazine interview", "personal_web"),
    ("newsletter_essay", "a personal newsletter essay", "essays_ideas"),
    ("fable", "a fable", "fiction"),
];

/// Native document types per row genre (two phrasings each).
pub fn native_types(genre: &str) -> [&'static str; 2] {
    match genre {
        "fiction" => ["a passage from a novel", "a passage of short-story prose"],
        "essays_ideas" => ["a passage from an essay", "a passage of discursive nonfiction"],
        "history_biography" => ["a passage from a history book", "a passage from a biography"],
        "science_nature" => ["a passage of popular science writing", "a passage of natural-history writing"],
        "travel" => ["a passage of travel writing", "a passage from a travel book"],
        "encyclopedia" => ["a section of an encyclopedia article", "an encyclopedia article section"],
        "news" => ["a news report", "a news article"],
        "government" => ["a passage from a US federal government notice or rule", "a passage from a federal regulatory notice"],
        "science" => ["a scientific research abstract", "a passage from a scientific research article"],
        _ => ["", ""],
    }
}

/// Native mirror phrasings: genre-specific for the nine source genres, else the
/// domain's protocol register (both phrasings).
pub fn native_types_for(genre: &str, domain: &str) -> [String; 2] {
    let t = native_types(genre);
    if t[0].is_empty() {
        let r = plan::register(domain).to_owned();
        [r.clone(), r]
    } else {
        [t[0].to_owned(), t[1].to_owned()]
    }
}

/// Row genre -> brief genre (the ten library genres).
pub fn brief_genre(genre: &str) -> &'static str {
    match genre {
        "fiction" => "fiction",
        "essays_ideas" | "student_essay" => "essays_ideas",
        "history_biography" | "historical_transcription" | "letters_diaries_manuscripts" => "history_biography",
        "science_nature" | "science_blog" => "science_nature",
        "travel" | "travel_guide" => "travel",
        "encyclopedia" | "textbook" => "encyclopedia",
        "news" | "news_citizen_media" | "historical_newspapers_us" | "historical_newspapers_uk" => "news",
        "government" | "legal" | "government_speech" => "government",
        "science" | "peer_review" => "science",
        _ => "personal_web",
    }
}

pub const HUMANIZE: [&str; 3] = [
    "Rewrite the following passage so it sounds as though a human wrote it, not an AI. Avoid common AI writing tells and formulaic phrasing. Vary rhythm naturally without inserting deliberate errors or inventing personal experiences. Keep its meaning, facts and approximate length.",
    "Humanize this text: make it read naturally, the way a person would actually write it, and remove anything that sounds machine-generated, such as stock transitions, hedging filler and overly balanced sentences. Keep every fact, the meaning and the approximate length.",
    "Paraphrase this passage so it would not read as AI-generated: use natural, varied phrasing and sentence lengths, and avoid predictable wording. Do not add errors, opinions or new information; keep the meaning, facts and approximate length.",
];
pub const FIX_SLOP_LEAD: &str = "Apply the pinned fix-slop editing skill below, including its phrase and construction checks. Its original scope is technical prose; use applicable guidance while preserving this passage's genre and meaning. Return only the edited passage, not an audit or explanation.";
pub const RETURN: &str = "Return only the replacement prose, retaining paragraph breaks where useful: no title, preface, analysis, quotation delimiters, or explanation.";

/// Format constraints (mirrors and full rewrites only).
pub const FORMATS: [&str; 16] = [
    "write it all in lowercase", "use plenty of emoji", "use bullet points", "write it as one long run-on paragraph",
    "use very short paragraphs of one or two sentences", "write it as a numbered list", "frame it as a question-and-answer exchange",
    "keep every sentence under ten words", "end with a P.S.", "use lots of parenthetical asides", "use no contractions at all",
    "use contractions and sentence fragments freely", "write in the second person", "write it as a sequence of short dated entries",
    "use a lot of exclamation marks", "write it in long, winding sentences",
];
/// Moods (any style-capable item).
pub const MOODS: [&str; 24] = [
    "excited", "grumpy", "anxious", "deadpan", "nostalgic", "giddy", "weary", "smug", "tender", "suspicious", "triumphant", "melancholy",
    "bewildered", "serene", "impatient", "conspiratorial", "awestruck", "sheepish", "indignant", "dreamy", "brisk", "wry", "jittery", "proud",
];
/// Audience modifiers (any style-capable item).
pub const AUDIENCES: [&str; 12] = [
    "as if explaining it to a curious ten-year-old", "as if writing to your oldest friend", "for a room of sceptical engineers",
    "as if reading it aloud at bedtime", "for a late-night radio audience", "as if leaving a voicemail", "for a busy boss who skims",
    "as if telling it at a campfire", "for a crowd at a town meeting", "as if writing on the back of a postcard",
    "for readers who are brand new to the subject", "as if confiding a secret",
];
/// Era and place settings for persona combinations.
pub const SETTINGS: [&str; 24] = [
    "in the 1920s", "in Victorian England", "in the far future", "in the Wild West", "on a long sea voyage", "in a medieval village",
    "in the 1980s", "on a space station", "in a small mountain town", "during a blackout", "at a seaside resort in the 1950s",
    "in a bustling night market", "at an Antarctic research base", "in a Renaissance court", "on a cross-country train",
    "in a 1970s newsroom", "in a sleepy fishing village", "in a big-city diner at 3 a.m.", "in ancient Rome", "in a jungle expedition camp",
    "at a county fair", "in a lighthouse during a storm", "in a monastery scriptorium", "in a roadside motel",
];
pub const VOICE_WRAPPERS: [&str; 5] = ["Make it {v}.", "Style: {v}.", "Write it so that you {v}.", "Voice: {v}.", "{V}."];

pub fn wrap_voice(v: &str, w: usize) -> String {
    let cap = {
        let mut c = v.chars();
        match c.next() {
            Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
            None => String::new(),
        }
    };
    VOICE_WRAPPERS[w % VOICE_WRAPPERS.len()].replace("{v}", v).replace("{V}", &cap)
}

pub fn key(x: &str, id: &str) -> String {
    text::sha_hex(format!("{V2_KEY_PREFIX}{x}:{id}").as_bytes())
}

/// Ranks of k_x(d) within (split, domain).
pub fn ranks(humans: &[&Human], x: &str) -> HashMap<String, usize> {
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

pub fn t_max(generator: &str) -> f64 {
    match generator {
        "qwen36" | "ernie_instruct" | "gptoss20b" => 1.2,
        "mistral7b" | "glm4" | "gemma3_4b" => 1.1,
        _ => 1.0,
    }
}

fn unit(h: &str, i: usize) -> f64 {
    u32::from_str_radix(&h[8 * i..8 * i + 8], 16).unwrap() as f64 / u32::MAX as f64
}
fn r3(x: f64) -> f64 {
    (x * 1000.0).round() / 1000.0
}

/// Per-item sampled decoding within the generator's range.
pub fn sample_decoding(item_key: &str, generator: &str, base: bool) -> Decoding {
    let h = text::sha_hex(format!("{V2_KEY_PREFIX}decoding:{item_key}").as_bytes());
    let (lo, hi) = if base { (0.6, 1.0) } else { (0.5, t_max(generator)) };
    let t = r3(lo + unit(&h, 0) * (hi - lo));
    let top_p = r3(0.85 + unit(&h, 1) * 0.15);
    let mut min_p = r3(unit(&h, 2) * 0.1);
    if t > 1.0 {
        min_p = min_p.max(0.05);
    }
    let rp = r3(1.0 + unit(&h, 3) * 0.15);
    Decoding {
        profile: if base { "v2_sampled_base".into() } else { "v2_sampled".into() },
        temperature: t,
        top_p,
        top_k: 0,
        min_p,
        repeat_penalty: rp,
        max_tokens: if base { 1024 } else { 1536 },
    }
}

/// One library v2 item (voice prompt or free-form brief).
#[derive(Clone, Debug)]
pub struct Brief {
    pub id: String,
    pub kind: String,
    /// brief genre, or "any" for voice prompts
    pub genre: String,
    pub inventor: String,
    pub instruction: String,
    pub partition: String,
    pub axis: String,
    pub imitated_author: Option<String>,
    pub rewrite_safe: bool,
    pub eval_author_conflict: bool,
    /// invented | human_derived
    pub origin: String,
    /// source family of a human-derived item (never applied to that family)
    pub source_family: Option<String>,
    pub cluster: String,
}

/// Ladder rung style kinds (rotated per ladder).
pub const LADDER_KINDS: [&str; 4] = ["voice", "persona_combo", "brief", "voice_plus"];
pub const LADDER_RATE: usize = 8;
pub const CROSS_RATE: usize = 12;

/// Library lists keyed by (partition, kind, genre, for_rewrite), interleaved
/// by axis (voices) or inventor (briefs) so round-robin spreads them evenly.
/// Rewrite lists hold only rewrite-safe items; Train lists exclude items whose
/// imitated author also appears among evaluation authors.
pub fn index_library(briefs: &[Brief]) -> BTreeMap<(String, String, String, bool), Vec<Brief>> {
    let mut by: BTreeMap<(String, String, String, bool), BTreeMap<String, Vec<Brief>>> = BTreeMap::new();
    for b in briefs {
        if b.partition == "train" && b.eval_author_conflict {
            continue;
        }
        let spread = if b.kind == "voice" { b.axis.clone() } else { b.inventor.clone() };
        for rw in [false, true] {
            if rw && !b.rewrite_safe {
                continue;
            }
            by.entry((b.partition.clone(), b.kind.clone(), b.genre.clone(), rw)).or_default().entry(spread.clone()).or_default().push(b.clone());
        }
    }
    let mut out = BTreeMap::new();
    for (k, inv) in by {
        let lists: Vec<Vec<Brief>> = inv.into_values().collect();
        let n = lists.iter().map(Vec::len).max().unwrap_or(0);
        let mut v = Vec::new();
        for i in 0..n {
            for l in &lists {
                if let Some(b) = l.get(i) {
                    v.push(b.clone());
                }
            }
        }
        out.insert(k, v);
    }
    out
}

/// Style kinds for style-capable items, by rank(k_style_kind) mod 20:
/// 0-7 voice, 8-10 voice_plus, 11-12 persona_combo, 13-19 brief.
pub fn style_kind(none_rank: usize, kind_rank: usize) -> &'static str {
    if none_rank % 4 == 0 {
        return "none";
    }
    match kind_rank % 20 {
        0..=7 => "voice",
        8..=10 => "voice_plus",
        11..=12 => "persona_combo",
        _ => "brief",
    }
}

/// Compose the exact style text from the assigned voice/brief and modifiers.
pub fn compose_style(st: &Value, voice: Option<&str>, brief: Option<&str>) -> Option<String> {
    let w = st["wrapper"].as_u64().unwrap_or(0) as usize;
    match st["style_kind"].as_str()? {
        "voice" => Some(wrap_voice(voice?, w)),
        "voice_plus" => {
            let m = &st["modifier"];
            let tail = match m["type"].as_str()? {
                "format" => format!("Also, {}.", m["text"].as_str()?),
                "mood" => format!("Keep the mood {}.", m["text"].as_str()?),
                _ => format!("Pitch it {}.", m["text"].as_str()?),
            };
            Some(format!("{} {tail}", wrap_voice(voice?, w)))
        }
        "persona_combo" => Some(format!(
            "{} Imagine that voice {}, and keep the mood {}.",
            wrap_voice(voice?, w),
            st["setting"].as_str()?,
            st["mood"].as_str()?
        )),
        "brief" => Some(format!("Follow this style brief: {}", brief?)),
        _ => None,
    }
}

pub fn mirror_prompt(doc_phrase: &str, native: bool, h: &Human, terms: &[String], style: Option<&str>, style_first: bool) -> String {
    let topic = match &h.title {
        Some(t) if native => format!(" Title or subject: {t}."),
        Some(t) => format!(" Topic: {t}."),
        None => String::new(),
    };
    let tail = if native {
        "Return only the passage, with no title, heading, preface, analysis, or explanation."
    } else {
        "Return only the text itself, with no title line, preface, analysis, or explanation."
    };
    let task = format!("Write {doc_phrase} in English, about {} words long.{topic} Cover these key terms: {}.", h.word_count, terms.join(", "));
    match style {
        None => format!("{task} Write natural, original text. {tail}"),
        Some(s) if style_first => format!("{s}\n\n{task} {tail}"),
        Some(s) => format!("{task} {s} {tail}"),
    }
}

/// Rewrite prompt for one span. Returns (prompt, target words).
#[allow(clippy::too_many_arguments)]
pub fn rewrite_prompt(
    op: &str,
    span: &str,
    prefix: &str,
    genre_desc: &str,
    style: Option<&str>,
    variant: usize,
    british: bool,
    fix_slop: &str,
) -> (String, usize) {
    let w = text::words(span);
    let sty = style.map(|s| format!("{s} Keep every fact and the meaning. ")).unwrap_or_default();
    match op {
        "paraphrase" => match style {
            None => (
                format!("Rewrite the following English passage in your own words while preserving its meaning, facts, perspective, register, and approximate length. {RETURN}\n\n{span}"),
                w,
            ),
            Some(_) => (
                format!("Rewrite the following English passage in a new voice while preserving its meaning, facts and approximate length. {sty}{RETURN}\n\n{span}"),
                w,
            ),
        },
        "spelling" => {
            let v = if british { "British" } else { "American" };
            (
                format!("Rewrite the following English passage in your own words while preserving its meaning, facts, perspective, register, and approximate length. Use consistent {v} English spelling, punctuation and usage throughout. {RETURN}\n\n{span}"),
                w,
            )
        }
        "polish" => (
            format!("Proofread and polish the following passage: correct grammar, spelling and punctuation, and smooth awkward or unclear phrasing, while keeping its meaning, facts, voice and length. {RETURN}\n\n{span}"),
            w,
        ),
        "expand" => {
            let n = ((w as f64) * 1.4).round() as usize;
            (
                format!("Expand the following passage to about {n} words. Elaborate on what it already says with fuller explanation and smoother transitions; do not add new facts, names, numbers, quotations or claims. {sty}{RETURN}\n\n{span}"),
                n,
            )
        }
        "condense" => {
            let n = (((w as f64) * 0.65).round() as usize).max(1);
            (format!("Condense the following passage to about {n} words, keeping its key facts, meaning and perspective. {sty}{RETURN}\n\n{span}"), n)
        }
        "formalize" => (
            format!("Rewrite the following passage in a more formal register, preserving its meaning, facts, perspective and approximate length. {RETURN}\n\n{span}"),
            w,
        ),
        "casual" => (
            format!("Rewrite the following passage in a relaxed, conversational tone, the way someone might put it informally, preserving its meaning, facts and approximate length. {sty}{RETURN}\n\n{span}"),
            w,
        ),
        "simplify" => (
            format!("Rewrite the following passage in plain, simple language that a general reader could follow easily, preserving all of its facts, its meaning and its approximate length. {sty}{RETURN}\n\n{span}"),
            w,
        ),
        "humanize" => (format!("{} {sty}{RETURN}\n\n{span}", HUMANIZE[variant % HUMANIZE.len()]), w),
        "fix_slop" => (format!("{FIX_SLOP_LEAD}\n{fix_slop}\n\nPASSAGE TO EDIT:\n{span}"), w),
        "continue" => {
            let voice = match style {
                Some(s) => format!(" {s} Stay consistent with what has been said."),
                None => " Continue in the same voice, consistent with what has been said.".to_owned(),
            };
            (
                format!("Here is the beginning of {genre_desc}. Continue it with the next part, about {w} words long.{voice} Return only the continuation, with no preface, analysis, or explanation, and do not repeat the beginning.\n\nBEGINNING:\n{prefix}"),
                w,
            )
        }
        _ => unreachable!("unknown op {op}"),
    }
}

/// Word-level change fraction 1 - LCS/max(len) between source and replacement.
pub fn change_fraction(a: &str, b: &str) -> f64 {
    let x: Vec<&str> = a.split_whitespace().collect();
    let y: Vec<&str> = b.split_whitespace().collect();
    if x.is_empty() && y.is_empty() {
        return 0.0;
    }
    let mut prev = vec![0u32; y.len() + 1];
    let mut cur = vec![0u32; y.len() + 1];
    for xi in &x {
        for (j, yj) in y.iter().enumerate() {
            cur[j + 1] = if xi == yj { prev[j] + 1 } else { prev[j + 1].max(cur[j]) };
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    1.0 - prev[y.len()] as f64 / x.len().max(y.len()) as f64
}

pub struct V2Inputs<'a> {
    pub humans: &'a [Human],
    pub idf: &'a HashMap<&'static str, Idf>,
    pub briefs: &'a [Brief],
    pub fix_slop: &'a str,
}

fn gen_for(split: &str, slot: usize) -> &'static str {
    match split {
        "train" => plan::TRAIN_INSTRUCT[slot % 8],
        "cal" => plan::eval_generator("cal", slot % 21),
        _ => plan::FAMILY_ORDER[slot % 7],
    }
}

/// Style fields for one style-capable item (library ids are filled later).
pub fn style_fields(kind: &str, r: &dyn Fn(&str) -> usize, allow_format: bool, brief_genre: &str) -> Value {
    let mut st = json!({"style_policy":STYLE_POLICY,"style_kind":kind,"wrapper":r("style_wrapper") % VOICE_WRAPPERS.len(),
        "style_first": r("style_pos") % 2 == 0,"voice_id":null,"brief_id":null,"brief_genre":null});
    match kind {
        "voice_plus" => {
            let m = r("style_mod");
            let t = if allow_format { m % 3 } else { 1 + m % 2 };
            st["modifier"] = match t {
                0 => json!({"type":"format","text":FORMATS[(m / 3) % FORMATS.len()]}),
                1 => json!({"type":"mood","text":MOODS[(m / 3) % MOODS.len()]}),
                _ => json!({"type":"audience","text":AUDIENCES[(m / 3) % AUDIENCES.len()]}),
            };
        }
        "persona_combo" => {
            st["setting"] = json!(SETTINGS[r("style_setting") % SETTINGS.len()]);
            st["mood"] = json!(MOODS[r("style_mod") % MOODS.len()]);
        }
        "brief" => st["brief_genre"] = json!(brief_genre),
        _ => {}
    }
    st
}

pub fn build_v2(inp: &V2Inputs) -> Result<Plan> {
    // 1. protocol primary cohorts on evaluation splits, unchanged.
    let eval_h: Vec<Human> = inp
        .humans
        .iter()
        .filter(|h| h.split != "train")
        .map(|h| Human {
            split: h.split.clone(),
            domain: h.domain,
            id: h.id.clone(),
            text: h.text.clone(),
            text_sha256: h.text_sha256.clone(),
            word_count: h.word_count,
            title: h.title.clone(),
            source_group: h.source_group.clone(),
            genre: h.genre.clone(),
        })
        .collect();
    let mut out = plan::build(&eval_h, inp.idf)?;
    // 2. diverse cohorts on every split; base cohort on Train.
    let lib = index_library(inp.briefs);
    let stop = text::stopwords();
    let all: Vec<&Human> = inp.humans.iter().collect();
    let names = [
        "mirror_gen", "mirror_doc", "mirror_none", "mirror_kind", "mixed_gen", "mixed_bin", "mixed_op", "mixed_none", "mixed_kind", "multi_span",
        "base", "variant", "ladder", "ladder_kind", "cross",
    ];
    let rk: HashMap<&str, HashMap<String, usize>> = names.iter().map(|x| (*x, ranks(&all, x))).collect();
    // library demand collected first, assigned round-robin afterwards:
    // (sort key, list key, item index, field)
    let mut wants: Vec<(String, (String, String, String, bool), usize, &'static str, Option<String>)> = Vec::new();
    let mut inherit: Vec<(usize, usize)> = Vec::new();
    let mut items: Vec<Item> = Vec::new();
    let mut failures = Vec::new();
    for h in &all {
        let r = |x: &str| rk[x][&h.id];
        let mut ranks_rec = BTreeMap::new();
        for x in names {
            ranks_rec.insert(format!("v2:{x}"), r(x));
        }
        let partition = h.split.clone();
        let base_item = |cohort: &str, kind: &str, generator: &str, tmpl: &str, base: bool| {
            let item_id = format!("pos-{}", &text::sha_hex(format!("v2:{}:{}:{}", h.split, h.id, cohort).as_bytes())[..24]);
            let dec = sample_decoding(&item_id, generator, base);
            Item {
                item_id,
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
                decoding: dec,
                template_id: tmpl.into(),
                source_spans: vec![],
                requested_fraction: None,
                requests: vec![],
                notes: vec![],
                ranks: ranks_rec.clone(),
                operation: None,
                style: None,
            }
        };
        // per-item keyed ranks for style sub-choices (wrapper, modifier, setting)
        let sub = |item_id: &str| {
            let item_id = item_id.to_owned();
            move |x: &str| -> usize {
                let k = key(&format!("style:{x}"), &item_id);
                u64::from_str_radix(&k[..12], 16).unwrap() as usize
            }
        };
        let train = h.split == "train";
        let push_wants = |it: &Item, st: &Value, rewrite: bool, wants: &mut Vec<_>, idx: usize| {
            let kind = st["style_kind"].as_str().unwrap();
            let group = st["ladder_id"].as_str().map(String::from);
            if matches!(kind, "voice" | "voice_plus" | "persona_combo") {
                wants.push((key("voice_order", &it.item_id), (partition.clone(), "voice".to_string(), "any".to_string(), rewrite), idx, "voice", group));
            } else if kind == "brief" {
                let g = st["brief_genre"].as_str().unwrap().to_owned();
                wants.push((key("brief_order", &it.item_id), (partition.clone(), "brief".to_string(), g, rewrite), idx, "brief", group));
            }
        };
        let ladder = (h.split == "train" || h.split == "dev_iter") && r("ladder") % LADDER_RATE == 0;
        let ladder_id = format!("ladder-{}", &key("ladder_id", &h.id)[..16]);
        // mirror
        let g = gen_for(&h.split, r("mirror_gen"));
        let mut it = base_item("diverse_mirror", "mirror", g, "a_comp_v2_mirror", false);
        let nt = native_types_for(&h.genre, h.domain);
        let bg_row = brief_genre(&h.genre).to_string();
        let ds = r("mirror_doc") % (2 * OFF_TYPES.len());
        let (doc_id, doc_phrase, native, bgenre) = if ds < OFF_TYPES.len() {
            ("native".to_string(), nt[ds % 2].clone(), true, bg_row.clone())
        } else {
            let t = OFF_TYPES[ds - OFF_TYPES.len()];
            (t.0.to_string(), t.1.to_string(), false, t.2.to_string())
        };
        let kind = style_kind(r("mirror_none"), r("mirror_kind"));
        let mut st = style_fields(kind, &sub(&it.item_id), true, &bgenre);
        st["document_type"] = json!(doc_id);
        st["document_phrase"] = json!(doc_phrase);
        st["native_document_type"] = json!(native);
        st["default_voice"] = json!(kind == "none");
        if ladder {
            st["ladder_id"] = json!(ladder_id);
            st["ladder_rung"] = json!(0);
        }
        push_wants(&it, &st, false, &mut wants, items.len());
        it.style = Some(st);
        let mirror_base = it.clone();
        items.push(it);
        if ladder {
            // one extra mirror rung: same passage topic, same generator and decoding, a different voice
            let mut x = mirror_base.clone();
            x.item_id = format!("pos-{}", &text::sha_hex(format!("v2:{}:{}:ladder_mirror_1", h.split, h.id).as_bytes())[..24]);
            x.cohort = "ladder_mirror".into();
            let kind = LADDER_KINDS[(r("ladder_kind") + 1) % LADDER_KINDS.len()];
            let base_st = mirror_base.style.clone().unwrap();
            let mut st = style_fields(kind, &sub(&x.item_id), true, &bgenre);
            for k in ["document_type", "document_phrase", "native_document_type"] {
                st[k] = base_st[k].clone();
            }
            st["default_voice"] = json!(false);
            st["ladder_id"] = json!(ladder_id);
            st["ladder_rung"] = json!(1);
            st["ladder_base_item"] = json!(mirror_base.item_id);
            push_wants(&x, &st, false, &mut wants, items.len());
            x.style = Some(st);
            items.push(x);
        }
        // mixed rewrite
        let g = gen_for(&h.split, r("mixed_gen"));
        let mut op = OP_SLOTS[r("mixed_op") % OP_SLOTS.len()];
        let bin = BINS[r("mixed_bin") % 7];
        let mut it = base_item("diverse_mixed", if bin == "R6" { "full" } else { "span" }, g, "a_comp_v2_rewrite", false);
        if op == "continue" && bin == "R6" {
            it.notes.push("continue_needs_human_prefix_full_bin_uses_paraphrase".into());
            op = "paraphrase";
        }
        it.operation = Some(op.into());
        let sents = text::sentences(&h.text);
        let cum = plan::prefix_words(&sents, &h.text);
        let multi = r("multi_span") % 3 == 0 && op != "continue";
        if op == "continue" {
            plan_continue(&mut it, h, &sents, &cum, bin, &mut failures);
        } else {
            plan::plan_mixed(&mut it, h, &sents, &cum, bin, multi, &mut failures);
        }
        let variant = r("variant");
        let kind = if BRIEF_OPS.contains(&op) { style_kind(r("mixed_none"), r("mixed_kind")) } else { "operation_only" };
        let mut st = if kind == "operation_only" {
            json!({"style_policy":STYLE_POLICY,"style_kind":"operation_only","voice_id":null,"brief_id":null})
        } else {
            style_fields(kind, &sub(&it.item_id), bin == "R6", &bg_row)
        };
        st["operation"] = json!(op);
        st["default_voice"] = json!(kind == "none" && matches!(op, "paraphrase" | "expand" | "condense" | "continue") || op == "polish");
        st["humanize_variant"] = if op == "humanize" { json!(variant % 3) } else { json!(null) };
        st["spelling_variant"] = if op == "spelling" { json!(if variant % 2 == 0 { "british" } else { "american" }) } else { json!(null) };
        if !it.source_spans.is_empty() {
            if ladder {
                st["ladder_id"] = json!(ladder_id);
                st["ladder_rung"] = json!(0);
            }
            push_wants(&it, &st, true, &mut wants, items.len());
            it.style = Some(st);
            let base_idx = items.len();
            let base = it.clone();
            items.push(it);
            if ladder {
                // three rewrite rungs: same spans, generator and decoding; distinct voices
                let rung_op = if BRIEF_OPS.contains(&op) { op } else { "paraphrase" };
                for k in 1..=3usize {
                    let mut x = base.clone();
                    x.item_id = format!("pos-{}", &text::sha_hex(format!("v2:{}:{}:ladder_rewrite_{k}", h.split, h.id).as_bytes())[..24]);
                    x.cohort = "ladder_rewrite".into();
                    x.operation = Some(rung_op.into());
                    let kind = LADDER_KINDS[(r("ladder_kind") + k + 1) % LADDER_KINDS.len()];
                    let mut st = style_fields(kind, &sub(&x.item_id), bin == "R6", &bg_row);
                    st["operation"] = json!(rung_op);
                    st["default_voice"] = json!(false);
                    st["humanize_variant"] = if rung_op == "humanize" { json!(variant % 3) } else { json!(null) };
                    st["spelling_variant"] = json!(null);
                    st["ladder_id"] = json!(ladder_id);
                    st["ladder_rung"] = json!(k + 1);
                    st["ladder_base_item"] = json!(base.item_id);
                    if rung_op != op {
                        x.notes.push("ladder_rung_uses_paraphrase_because_base_operation_has_no_voice".into());
                    }
                    push_wants(&x, &st, true, &mut wants, items.len());
                    x.style = Some(st);
                    items.push(x);
                }
            }
            if train && r("cross") % CROSS_RATE == 0 {
                // same passage, spans, operation and voice under two other generators
                let gi = plan::TRAIN_INSTRUCT.iter().position(|x| *x == base.generator).unwrap_or(0);
                for (k, off) in [(1usize, 3usize), (2, 5)] {
                    let g2 = plan::TRAIN_INSTRUCT[(gi + off) % 8];
                    let mut x = base.clone();
                    x.item_id = format!("pos-{}", &text::sha_hex(format!("v2:{}:{}:cross_gen_{k}", h.split, h.id).as_bytes())[..24]);
                    x.cohort = "cross_generator".into();
                    x.generator = g2.into();
                    x.decoding = sample_decoding(&x.item_id, g2, false);
                    let mut st = base.style.clone().unwrap();
                    st["cross_id"] = json!(format!("cross-{}", &key("cross_id", &h.id)[..16]));
                    st["cross_base_item"] = json!(base.item_id);
                    st["cross_base_generator"] = json!(base.generator);
                    if let Some(o) = st.as_object_mut() {
                        o.remove("ladder_id");
                        o.remove("ladder_rung");
                    }
                    x.style = Some(st);
                    inherit.push((items.len(), base_idx));
                    items.push(x);
                }
            }
        }
        // Train base cohort
        if train && r("base") % 4 == 0 {
            let rb = r("base");
            let g = plan::TRAIN_BASE[(rb / 8) % 3];
            let cont = (rb / 4) % 2 == 0;
            let kind = if cont { "continuation" } else { "splice" };
            let tmpl = if cont { plan::BASE_CONT_TEMPLATE_ID } else { plan::BASE_SPLICE_TEMPLATE_ID };
            let mut it = base_item("train_base_v2", kind, g, tmpl, true);
            it.style = Some(json!({"style_policy":STYLE_POLICY,"style_kind":"base_checkpoint","operation":kind,"voice_id":null,"brief_id":null,"default_voice":true}));
            let n = sents.len();
            let total = h.word_count;
            let need = if cont { 30 } else { 25 };
            let j = (1..=n).find(|&j| cum[j] * 100 >= need * total);
            let pl = match j {
                Some(j) if j < n => {
                    if cont {
                        Some((j, n))
                    } else {
                        match (0..n).rev().find(|&i| (total - cum[i]) * 100 >= 25 * total) {
                            Some(i2) if j < i2 => Some((j, i2)),
                            _ => None,
                        }
                    }
                }
                _ => None,
            };
            match pl {
                Some((a, b)) => {
                    let (s, e) = (sents[a].start, sents[b - 1].end);
                    it.source_spans = vec![(s, e)];
                    it.requested_fraction = Some(text::words(&h.text[s..e]) as f64 / total as f64);
                    items.push(it);
                }
                None => failures.push(json!({"item_id":it.item_id,"split":h.split,"parent_id":h.id,"cohort":it.cohort,
                    "generator":g,"kind":kind,"stage":"plan","reason":"no_valid_prefix_suffix_plan","sentences":n})),
            }
        }
    }
    // round-robin library assignment within each list; skip an item whose
    // human-derived source family is the parent's, or whose cluster is
    // already used in the same voice ladder
    wants.sort();
    let mut cursor: HashMap<(String, String, String, bool), usize> = HashMap::new();
    let mut ladder_clusters: HashMap<String, std::collections::HashSet<String>> = HashMap::new();
    let parent_family: HashMap<String, String> = all.iter().map(|h| (h.id.clone(), h.source_group.clone())).collect();
    let mut relaxed = 0usize;
    for (_, lk, idx, field, group) in &wants {
        let list = lib.get(lk).with_context(|| format!("no library items for {lk:?}"))?;
        ensure!(!list.is_empty(), "empty library list {lk:?}");
        let c = cursor.entry(lk.clone()).or_default();
        let fam = &parent_family[&items[*idx].parent_id];
        let used = group.as_ref().and_then(|g| ladder_clusters.get(g));
        let ok = |b: &Brief| b.source_family.as_deref() != Some(fam.as_str()) && used.is_none_or(|u| !u.contains(&b.cluster));
        let mut pick = None;
        for d in 0..list.len() {
            let b = &list[(*c + d) % list.len()];
            if ok(b) {
                pick = Some(d);
                break;
            }
        }
        let d = match pick {
            Some(d) => d,
            None => {
                // only the ladder-distinctness constraint may be relaxed; never the source-family rule
                let mut f = None;
                for d in 0..list.len() {
                    if list[(*c + d) % list.len()].source_family.as_deref() != Some(fam.as_str()) {
                        f = Some(d);
                        break;
                    }
                }
                relaxed += 1;
                f.context("every library item shares the parent's source family")?
            }
        };
        let b = &list[(*c + d) % list.len()];
        *c += d + 1;
        if let Some(g) = group {
            ladder_clusters.entry(g.clone()).or_default().insert(b.cluster.clone());
        }
        let st = items[*idx].style.as_mut().unwrap();
        st[format!("{field}_id")] = json!(b.id);
        st[format!("{field}_text")] = json!(b.instruction);
        st[format!("{field}_inventor")] = json!(b.inventor);
        st[format!("{field}_axis")] = json!(b.axis);
        st[format!("{field}_partition")] = json!(b.partition);
        st[format!("{field}_imitated_author")] = json!(b.imitated_author);
        st[format!("{field}_cluster")] = json!(b.cluster);
        st[format!("{field}_origin")] = json!(b.origin);
        st[format!("{field}_source_family")] = json!(b.source_family);
    }
    // cross-generator items inherit the base item's assigned style exactly
    for (x, b) in &inherit {
        let base = items[*b].style.clone().unwrap();
        let st = items[*x].style.as_mut().unwrap();
        for (k, v) in base.as_object().unwrap() {
            if k.starts_with("voice_") || k.starts_with("brief_") {
                st[k] = v.clone();
            }
        }
    }
    if relaxed > 0 {
        failures.push(json!({"stage":"plan","reason":"ladder_cluster_distinctness_relaxed","count":relaxed}));
    }
    // requests
    let hmap: HashMap<&str, &Human> = all.iter().map(|h| (h.id.as_str(), *h)).collect();
    for it in items.iter_mut() {
        let h = hmap[it.parent_id.as_str()];
        let st = it.style.clone().unwrap();
        let style = compose_style(&st, st["voice_text"].as_str(), st["brief_text"].as_str());
        if let Some(s) = &style {
            it.style.as_mut().unwrap()["style_text"] = json!(s);
        }
        let mut reqs: Vec<(String, String, Option<(usize, usize)>, usize, Option<usize>)> = Vec::new();
        match it.kind.as_str() {
            "mirror" => {
                let terms = text::key_terms(&h.text, &inp.idf[h.domain], &stop, 8);
                let p = mirror_prompt(st["document_phrase"].as_str().unwrap(), st["native_document_type"] == true, h, &terms, style.as_deref(),
                    st["style_first"] == true);
                reqs.push(("chat".into(), p, None, h.word_count, None));
            }
            "continuation" | "splice" => {
                let (s, e) = it.source_spans[0];
                let sents = text::sentences(&h.text);
                let a = sents.iter().position(|x| x.start == s).context("span start")?;
                let prefix = h.text[..sents[a - 1].end].to_owned();
                let removed = text::words(&h.text[s..e]);
                let cum_before = text::words(&h.text[..s]);
                let budget = if it.kind == "continuation" { h.word_count - cum_before } else { removed };
                reqs.push(("completion".into(), prefix, Some((s, e)), removed, Some(budget)));
            }
            _ => {
                let op = it.operation.clone().unwrap();
                let gd = match crate::briefs::genre_desc(&h.genre) {
                    Some(g) => format!("a passage of {g}"),
                    None => plan::register(h.domain).to_owned(),
                };
                for &(s, e) in &it.source_spans {
                    let prefix = h.text[..s].trim_end();
                    let (p, tw) = rewrite_prompt(
                        &op,
                        &h.text[s..e],
                        prefix,
                        &gd,
                        style.as_deref(),
                        st["humanize_variant"].as_u64().unwrap_or(0) as usize,
                        st["spelling_variant"] == "british",
                        inp.fix_slop,
                    );
                    reqs.push(("chat".into(), p, Some((s, e)), tw, None));
                }
            }
        }
        for (endpoint, prompt, span, target, budget) in reqs {
            let tmpl = format!("{}:{}", it.template_id, it.operation.clone().unwrap_or_else(|| it.kind.clone()));
            let rs = plan::request_sha(&it.generator, &endpoint, &tmpl, &prompt, &it.decoding);
            let req = Request {
                request_id: format!("req-{}", &text::sha_hex(format!("{}:{}", it.item_id, it.requests.len()).as_bytes())[..24]),
                item_id: it.item_id.clone(),
                parent_id: it.parent_id.clone(),
                split: it.split.clone(),
                generator: it.generator.clone(),
                endpoint,
                template_id: it.template_id.clone(),
                prompt,
                span_index: it.requests.len(),
                source_span: span,
                target_words: target,
                cut_budget: budget,
                decoding: it.decoding.clone(),
                seed: plan::seed_of(&rs),
                request_sha256: rs,
                op: it.operation.clone(),
                cohort: Some(it.cohort.clone()),
                attempt_limit: None,
            };
            it.requests.push(req.request_id.clone());
            out.requests.push(req);
        }
    }
    out.items.extend(items);
    out.failures.extend(failures);
    Ok(out)
}

/// Cramér's V between two categorical labels (bias-uncorrected).
pub fn cramers_v(pairs: &[(String, String)]) -> f64 {
    let n = pairs.len() as f64;
    if n == 0.0 {
        return 0.0;
    }
    let mut a: HashMap<&str, f64> = HashMap::new();
    let mut b: HashMap<&str, f64> = HashMap::new();
    let mut ab: HashMap<(&str, &str), f64> = HashMap::new();
    for (x, y) in pairs {
        *a.entry(x).or_default() += 1.0;
        *b.entry(y).or_default() += 1.0;
        *ab.entry((x, y)).or_default() += 1.0;
    }
    let k = (a.len().min(b.len()) as f64 - 1.0).max(1.0);
    // chi2 = sum over observed cells (o^2/e) - n
    let mut s = 0.0;
    for ((x, y), o) in &ab {
        let e = a[x] * b[y] / n;
        s += o * o / e;
    }
    ((s - n).max(0.0) / (n * k)).sqrt()
}

/// Cramér's V with a fixed-seed permutation baseline of the second label
/// (the baseline shows the value expected from chance with many categories).
pub fn factorial_v(pairs: &[(String, String)], seed: &str) -> Value {
    let v = cramers_v(pairs);
    let mut ys: Vec<String> = pairs.iter().map(|p| p.1.clone()).collect();
    let mut rng = crate::library::Rng::new(seed);
    for i in (1..ys.len()).rev() {
        let j = rng.below(i + 1);
        ys.swap(i, j);
    }
    let perm: Vec<(String, String)> = pairs.iter().map(|p| p.0.clone()).zip(ys).collect();
    json!({"n":pairs.len(),"cramers_v":v,"permutation_baseline_v":cramers_v(&perm)})
}

/// Continue: the replaced run always ends the document (R0-R4: one run
/// anchored at the end; R5: keep the first sentence).
fn plan_continue(it: &mut Item, h: &Human, sents: &[text::Sentence], cum: &[usize], bin: &str, failures: &mut Vec<Value>) {
    it.bin = Some(bin.into());
    let n = sents.len();
    let run = match bin {
        "R5" => {
            it.anchor = Some("keep_start".into());
            if n >= 2 { Some((1, n)) } else { None }
        }
        _ => {
            let t = plan::bin_target_bp(bin).unwrap();
            it.target_bp = Some(t);
            it.anchor = Some("end".into());
            plan::single_run(cum, t, 2)
        }
    };
    match run {
        Some(r) if r.0 > 0 => {
            it.source_spans = vec![plan::byte_span(sents, r)];
            it.requested_fraction = Some((cum[r.1] - cum[r.0]) as f64 / cum[n] as f64);
        }
        _ => failures.push(json!({"item_id":it.item_id,"split":h.split,"parent_id":h.id,"cohort":it.cohort,
            "generator":it.generator,"bin":bin,"operation":"continue","stage":"plan","reason":"no_valid_continue_span_plan","sentences":n})),
    }
}

/// Pinned fix-slop bundle (sam0x17/fix-slop @ 78555d2f), hash-checked.
pub const FIX_SLOP_FILES: [(&str, &str); 4] = [
    ("SKILL.md", "d0bc56baf7144958ff5cafd271bc790680cd7531ba30bbeee36cb49789a8e538"),
    ("references/phrases.md", "321bc05f5b23dd7593ffafe0f433266e4600d4fe472f20267d8cf88e8cc921e1"),
    ("references/constructions.md", "8dbefaeb4359c045dc721bcd7679e11e4bfb75ff17c838a73b9fa0be6cf18cb2"),
    ("references/greps.txt", "910fcd2d58168a777edb8763d434c9a36a6eeb464eb27676873a4fdf23fab940"),
];
pub const FIX_SLOP_REVISION: &str = "https://github.com/sam0x17/fix-slop/blob/78555d2f173db50b8ca03a0cb0961cac4abee0e0/SKILL.md";

pub fn load_fix_slop(root: &std::path::Path) -> Result<String> {
    let mut b = String::new();
    for (name, hash) in FIX_SLOP_FILES {
        let bytes = std::fs::read(root.join(name))?;
        ensure!(text::sha_hex(&bytes) == hash, "pinned fix-slop file differs: {name}");
        b.push_str(&format!("\n\nFILE: {name}\n{}", std::str::from_utf8(&bytes)?));
    }
    Ok(b)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn decoding_in_range_and_stable() {
        for i in 0..200 {
            let k = format!("item{i}");
            for g in ["qwen36", "phi4mini", "glm4"] {
                let d = sample_decoding(&k, g, false);
                assert!(d.temperature >= 0.5 && d.temperature <= t_max(g));
                assert!(d.top_p >= 0.85 && d.top_p <= 1.0);
                assert!(d.min_p >= 0.0 && d.min_p <= 0.1);
                assert!(d.repeat_penalty >= 1.0 && d.repeat_penalty <= 1.15);
                if d.temperature > 1.0 {
                    assert!(d.min_p >= 0.05);
                }
                assert_eq!(d, sample_decoding(&k, g, false));
            }
            let b = sample_decoding(&k, "granite_base", true);
            assert!(b.temperature >= 0.6 && b.temperature <= 1.0 && b.max_tokens == 1024);
        }
    }
    #[test]
    fn cramers_v_bounds() {
        let ind: Vec<(String, String)> = (0..400).map(|i| (format!("g{}", i % 4), format!("v{}", (i / 4) % 5))).collect();
        assert!(cramers_v(&ind) < 0.05);
        let dep: Vec<(String, String)> = (0..400).map(|i| (format!("g{}", i % 4), format!("v{}", i % 4))).collect();
        assert!((cramers_v(&dep) - 1.0).abs() < 1e-9);
    }
    #[test]
    fn change_fraction_lcs() {
        assert_eq!(change_fraction("a b c d", "a b c d"), 0.0);
        assert!((change_fraction("a b c d", "a x c d") - 0.25).abs() < 1e-12);
        assert_eq!(change_fraction("a b", "c d"), 1.0);
    }
    #[test]
    fn prompts_carry_style_op_and_targets() {
        let (p, n) = rewrite_prompt("expand", "one two three four five six seven eight nine ten", "", "x", Some("Make it sound like a sailor."), 0, false, "");
        assert!(p.contains("sound like a sailor") && p.contains("about 14 words") && n == 14);
        let (p, n) = rewrite_prompt("condense", "one two three four five six seven eight nine ten", "", "x", None, 0, false, "");
        assert!(!p.contains("sailor") && n == 7);
        let (p, _) = rewrite_prompt("paraphrase", "a b", "", "x", Some("Voice: sound Scottish."), 0, false, "");
        assert!(p.contains("new voice") && p.contains("sound Scottish") && !p.contains("register"));
        let (p, _) = rewrite_prompt("paraphrase", "a b", "", "x", None, 0, false, "");
        assert!(p.contains("register"));
        let (p, _) = rewrite_prompt("spelling", "a b", "", "x", None, 0, true, "");
        assert!(p.contains("British"));
        let (p, _) = rewrite_prompt("continue", "a b", "PREFIX TEXT", "a passage of news report", Some("Style: sound like a cowboy."), 0, false, "");
        assert!(p.contains("PREFIX TEXT") && p.contains("cowboy") && !p.contains("\n\na b"));
        let (p, _) = rewrite_prompt("fix_slop", "a b", "", "x", None, 0, false, "SKILLBUNDLE");
        assert!(p.contains("SKILLBUNDLE"));
        for v in 0..3 {
            let (p, _) = rewrite_prompt("humanize", "a b", "", "x", None, v, false, "");
            assert!(p.contains(HUMANIZE[v]));
        }
    }
    #[test]
    fn style_kinds_wrappers_and_composition() {
        let mut n = BTreeMap::new();
        for a in 0..40 {
            for b in 0..20 {
                *n.entry(style_kind(a, b)).or_insert(0) += 1;
            }
        }
        assert_eq!(n["none"], 200);
        assert_eq!(n["voice"] + n["voice_plus"] + n["persona_combo"], 390);
        assert_eq!(n["brief"], 210);
        assert_eq!(wrap_voice("sound like a sailor", 0), "Make it sound like a sailor.");
        assert_eq!(wrap_voice("sound like a sailor", 4), "Sound like a sailor.");
        let st = json!({"style_kind":"persona_combo","wrapper":1,"setting":"in the 1920s","mood":"grumpy"});
        assert_eq!(compose_style(&st, Some("sound like a sailor"), None).unwrap(),
            "Style: sound like a sailor. Imagine that voice in the 1920s, and keep the mood grumpy.");
        let st = json!({"style_kind":"voice_plus","wrapper":3,"modifier":{"type":"format","text":"use bullet points"}});
        assert_eq!(compose_style(&st, Some("sound Scottish"), None).unwrap(), "Voice: sound Scottish. Also, use bullet points.");
        let st = json!({"style_kind":"brief","wrapper":0});
        assert!(compose_style(&st, None, Some("Long winding sentences.")).unwrap().starts_with("Follow this style brief:"));
        assert!(compose_style(&json!({"style_kind":"none"}), None, None).is_none());
        let r = |x: &str| x.len() * 7;
        let st = style_fields("voice_plus", &r, false, "news");
        assert_ne!(st["modifier"]["type"], "format");
    }
    #[test]
    fn library_round_robin_interleaves_and_filters() {
        let mk = |id: &str, axis: &str, safe: bool, conflict: bool| Brief {
            id: id.into(),
            kind: "voice".into(),
            genre: "any".into(),
            inventor: "q".into(),
            instruction: "sound like x".into(),
            partition: "train".into(),
            axis: axis.into(),
            imitated_author: None,
            rewrite_safe: safe,
            eval_author_conflict: conflict,
            origin: "invented".into(),
            source_family: None,
            cluster: format!("vc-{id}"),
        };
        let lib = index_library(&[mk("a1", "a", true, false), mk("a2", "a", false, false), mk("b1", "b", true, false), mk("c1", "c", true, true)]);
        let v: Vec<&str> = lib[&("train".into(), "voice".into(), "any".into(), false)].iter().map(|b| b.id.as_str()).collect();
        assert_eq!(v, vec!["a1", "b1", "a2"]);
        let v: Vec<&str> = lib[&("train".into(), "voice".into(), "any".into(), true)].iter().map(|b| b.id.as_str()).collect();
        assert_eq!(v, vec!["a1", "b1"]);
    }

}
