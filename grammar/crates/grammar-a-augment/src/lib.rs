//! Class-blind, seeded text noise for A.
//!
//! One noise model serves two uses:
//!
//! - **Augmented training copies.** A configured fraction of bundle rows of
//!   *every* origin (humans, mixed and model-only alike) gets one noised copy,
//!   so no noise type can become a class cue. The copy keeps its source
//!   group, split and labels; its model-written byte spans and realised share
//!   are remapped exactly, and the edits are recorded per row.
//! - **Consistency probes.** Exact repeats and lightly perturbed copies of
//!   evaluation rows, listed in a text-free manifest that the competitive-A
//!   evaluator reads to report how much P(involvement) moves.
//!
//! Class blindness: every random draw comes from SHA-256 of the noise model,
//! profile, seed, row key and token or gap index, and which operations apply
//! depends only on the token's own characters. Labels never enter a draw, so
//! the same text gets the same edits whatever its labels. The single
//! label-aware rule is a guard: a deletion is skipped (and counted) when it
//! would remove the last word of a labelled run, so a deletion never changes
//! a row's origin or empties a model span.
//!
//! Attribution: applying cheap character- and word-level noise (typos,
//! deletions, synonym swaps, zero-width characters) to both classes follows
//! an idea in the MIT-licensed It's AI validator
//! (<https://github.com/It-s-AI/llm-detection>, Copyright 2024 It's AI). The
//! operation set, rates, profiles, word lists, span remapping and records
//! here are our own; no code or lists were copied.
//!
//! Operations target our own robustness and diagnostic slices: keyboard
//! typos, dropped apostrophes and casing noise (informal web, forum, chat and
//! learner writing), OCR confusions (OCR-tier historical text), article and
//! word drops (learner essays, terse informal text), dropped clause
//! punctuation and whitespace or line-break noise (spoken transcripts and
//! removed markup), synonym swaps (light lexical variation), and zero-width
//! characters and homoglyphs (evasion attempts). The rates are declared
//! starting values, not tuned results; change them by a new profile name.
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

pub const RECORD_SCHEMA: &str = "slop_ninja_a_augmentation_v1";
pub const PROBE_SCHEMA: &str = "slop_ninja_a_consistency_probe_v1";
pub const NOISE_MODEL: &str = "a_noise_v1";
pub const WORD_POLICY: &str = "unicode_split_whitespace_v1";
const KEY_PREFIX: &str = "slopninja-a-augment-v1";

/// Word-level operations in draw order (at most one per word).
pub const TOKEN_OPS: [&str; 10] =
    ["typo", "ocr", "synonym", "article_drop", "word_drop", "apostrophe", "casing", "punct", "zero_width", "homoglyph"];
/// Gap-level operation (whitespace between two kept words).
pub const GAP_OP: &str = "whitespace";

/// A named noise profile: expected edits per 100 eligible words (per 100
/// eligible gaps for `whitespace`) at intensity 1, a per-row intensity drawn
/// uniformly in [intensity_min, intensity_max], and caps on edited words and
/// gaps as a fraction of the row's words.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Profile {
    pub name: String,
    pub per_100_words: BTreeMap<String, f64>,
    pub intensity_min: f64,
    pub intensity_max: f64,
    pub max_edit_fraction: f64,
    /// Minimum word edits per row (capped by the edit cap). When the random
    /// pass makes fewer, a keyed label-blind fallback edits further words with
    /// non-deleting operations, chosen in proportion to their rates.
    #[serde(default)]
    pub min_edits: usize,
}

impl Profile {
    /// Built-in profiles: `train_v1` (augmented training copies) and
    /// `light_v1` (consistency probes: a few surface edits per 100 words,
    /// no deletions, swaps or homoglyphs).
    pub fn builtin(name: &str) -> Result<Profile> {
        let (rates, lo, hi, cap): (&[(&str, f64)], f64, f64, f64) = match name {
            "train_v1" => (
                &[
                    ("typo", 1.5),
                    ("ocr", 0.3),
                    ("synonym", 0.8),
                    ("article_drop", 8.0),
                    ("word_drop", 0.4),
                    ("apostrophe", 6.0),
                    ("casing", 0.6),
                    ("punct", 4.0),
                    ("zero_width", 0.2),
                    ("homoglyph", 0.1),
                    ("whitespace", 1.0),
                ],
                0.25,
                2.0,
                0.12,
            ),
            "light_v1" => (&[("typo", 0.6), ("casing", 0.4), ("punct", 2.0), ("zero_width", 0.1), ("whitespace", 0.6)], 1.0, 1.0, 0.03),
            _ => bail!("unknown noise profile {name} (built in: train_v1, light_v1)"),
        };
        let p = Profile {
            name: name.into(),
            per_100_words: rates.iter().map(|(k, v)| (k.to_string(), *v)).collect(),
            intensity_min: lo,
            intensity_max: hi,
            max_edit_fraction: cap,
            min_edits: 1,
        };
        p.validate()?;
        Ok(p)
    }

    pub fn validate(&self) -> Result<()> {
        ensure!(!self.name.is_empty(), "profile name");
        for (k, v) in &self.per_100_words {
            ensure!(TOKEN_OPS.contains(&k.as_str()) || k == GAP_OP, "unknown noise operation {k}");
            ensure!(v.is_finite() && *v >= 0.0, "rate of {k} must be finite and non-negative");
        }
        ensure!(
            self.intensity_min.is_finite() && self.intensity_min >= 0.0 && self.intensity_max >= self.intensity_min,
            "intensity range"
        );
        ensure!((0.0..=1.0).contains(&self.max_edit_fraction), "max_edit_fraction in [0, 1]");
        // per-word probabilities at the highest intensity: each op <= 1 and the sum <= 1
        let total: f64 =
            TOKEN_OPS.iter().map(|o| self.per_100_words.get(*o).copied().unwrap_or(0.0) / 100.0 * self.intensity_max).sum();
        ensure!(total <= 1.0, "word-level operation probabilities sum to {total} > 1 at maximum intensity");
        ensure!(self.rate(GAP_OP) / 100.0 * self.intensity_max <= 1.0, "whitespace probability above 1");
        Ok(())
    }

    fn rate(&self, op: &str) -> f64 {
        self.per_100_words.get(op).copied().unwrap_or(0.0)
    }

    /// SHA-256 of the canonical JSON (maps are ordered).
    pub fn sha256(&self) -> String {
        hex::encode(Sha256::digest(serde_json::to_vec(self).unwrap()))
    }
}

/// SplitMix64 stream seeded from SHA-256 of the key parts.
struct Rng(u64);

impl Rng {
    fn keyed(parts: &[&str]) -> Rng {
        let h = Sha256::digest(parts.join("\0").as_bytes());
        Rng(u64::from_be_bytes(h[..8].try_into().unwrap()))
    }
    fn sub(base: &[&str], extra: &[&str]) -> Rng {
        let mut v = base.to_vec();
        v.extend_from_slice(extra);
        Rng::keyed(&v)
    }
    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    fn unit(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next_u64() % n as u64) as usize
    }
}

/// Keyed uniform [0, 1) used for class-blind row selection.
pub fn select_unit(salt: &str, seed: u64, row_id: &str) -> f64 {
    Rng::keyed(&[KEY_PREFIX, "select", salt, &seed.to_string(), row_id]).unit()
}

/// Whether a row is selected at `rate` (independent of its labels).
pub fn selected(salt: &str, seed: u64, row_id: &str, rate: f64) -> bool {
    select_unit(salt, seed, row_id) < rate
}

// ------------------------------------------------------------ word lists
// Our own lists (MIT, this repository). Directional single-word swaps chosen
// to be safe in most contexts; discourse markers are deliberately absent so a
// swap never adds or removes a typical model transition.
const SYNONYMS: &[(&str, &str)] = &[
    ("big", "large"),
    ("large", "big"),
    ("small", "little"),
    ("quick", "fast"),
    ("fast", "quick"),
    ("start", "begin"),
    ("begin", "start"),
    ("maybe", "perhaps"),
    ("perhaps", "maybe"),
    ("almost", "nearly"),
    ("nearly", "almost"),
    ("glad", "happy"),
    ("happy", "glad"),
    ("difficult", "hard"),
    ("easy", "simple"),
    ("choose", "pick"),
    ("street", "road"),
    ("road", "street"),
    ("talk", "speak"),
    ("purchase", "buy"),
    ("buy", "purchase"),
    ("children", "kids"),
    ("kids", "children"),
    ("child", "kid"),
    ("kid", "child"),
    ("mistake", "error"),
    ("error", "mistake"),
    ("reply", "answer"),
    ("clever", "smart"),
    ("smart", "clever"),
    ("afraid", "scared"),
    ("scared", "afraid"),
    ("wealthy", "rich"),
    ("enormous", "huge"),
    ("huge", "enormous"),
    ("frequently", "often"),
    ("often", "frequently"),
    ("rapidly", "quickly"),
    ("quickly", "rapidly"),
    ("sofa", "couch"),
    ("couch", "sofa"),
    ("journey", "trip"),
    ("ill", "sick"),
    ("sick", "ill"),
    ("rock", "stone"),
    ("stone", "rock"),
];
const ARTICLES: [&str; 3] = ["a", "an", "the"];
/// OCR confusions (pattern, replacement).
const OCR: &[(&str, &str)] = &[
    ("rn", "m"),
    ("m", "rn"),
    ("cl", "d"),
    ("d", "cl"),
    ("li", "h"),
    ("h", "li"),
    ("l", "1"),
    ("1", "l"),
    ("O", "0"),
    ("0", "O"),
    ("e", "c"),
    ("vv", "w"),
    ("w", "vv"),
];
/// Latin letter to a Cyrillic or Greek lookalike.
const HOMOGLYPHS: &[(char, char)] = &[
    ('a', '\u{0430}'),
    ('e', '\u{0435}'),
    ('o', '\u{043E}'),
    ('p', '\u{0440}'),
    ('c', '\u{0441}'),
    ('x', '\u{0445}'),
    ('y', '\u{0443}'),
    ('i', '\u{0456}'),
    ('A', '\u{0391}'),
    ('B', '\u{0392}'),
    ('E', '\u{0395}'),
    ('H', '\u{0397}'),
    ('K', '\u{039A}'),
    ('M', '\u{039C}'),
    ('O', '\u{039F}'),
    ('P', '\u{03A1}'),
    ('T', '\u{03A4}'),
    ('X', '\u{03A7}'),
];
const ZERO_WIDTH: [char; 4] = ['\u{200B}', '\u{200C}', '\u{200D}', '\u{2060}'];
const QWERTY: [&str; 3] = ["qwertyuiop", "asdfghjkl", "zxcvbnm"];

fn neighbours(c: char) -> Vec<char> {
    let lc = c.to_ascii_lowercase();
    let mut v = Vec::new();
    for (r, row) in QWERTY.iter().enumerate() {
        if let Some(i) = row.find(lc) {
            let b = row.as_bytes();
            if i > 0 {
                v.push(b[i - 1] as char);
            }
            if i + 1 < b.len() {
                v.push(b[i + 1] as char);
            }
            for rr in [r.wrapping_sub(1), r + 1] {
                if let Some(o) = QWERTY.get(rr) {
                    let ob = o.as_bytes();
                    for d in [i.wrapping_sub(1), i, i + 1] {
                        if let Some(&x) = ob.get(d) {
                            v.push(x as char);
                        }
                    }
                }
            }
        }
    }
    v.sort();
    v.dedup();
    v.retain(|x| *x != lc);
    if c.is_ascii_uppercase() { v.iter().map(|x| x.to_ascii_uppercase()).collect() } else { v }
}

/// (leading punctuation, core, trailing punctuation) of a word.
fn split_core(t: &str) -> (&str, &str, &str) {
    let s = t.find(|c: char| c.is_alphanumeric()).unwrap_or(t.len());
    let e = t.rfind(|c: char| c.is_alphanumeric()).map(|i| i + t[i..].chars().next().unwrap().len_utf8()).unwrap_or(s);
    if s >= e { (t, "", "") } else { (&t[..s], &t[s..e], &t[e..]) }
}

fn case_like(src: &str, word: &str) -> Option<String> {
    if src.chars().all(|c| c.is_ascii_lowercase()) {
        Some(word.to_owned())
    } else if src.len() > 1 && src.chars().all(|c| c.is_ascii_uppercase()) {
        Some(word.to_ascii_uppercase())
    } else if src.chars().next().is_some_and(|c| c.is_ascii_uppercase()) && src.chars().skip(1).all(|c| c.is_ascii_lowercase()) {
        let mut w = word.to_owned();
        w[..1].make_ascii_uppercase();
        Some(w)
    } else {
        None
    }
}

fn synonym(core: &str) -> Option<String> {
    let lc = core.to_ascii_lowercase();
    SYNONYMS.iter().find(|(a, _)| *a == lc).and_then(|(_, b)| case_like(core, b))
}

fn apostrophe_at(core: &str) -> Option<usize> {
    let cs: Vec<(usize, char)> = core.char_indices().collect();
    (1..cs.len().saturating_sub(1)).find(|&i| matches!(cs[i].1, '\'' | '\u{2019}') && cs[i - 1].1.is_alphabetic() && cs[i + 1].1.is_alphabetic()).map(|i| cs[i].0)
}

fn ocr_sites(core: &str) -> Vec<(usize, usize)> {
    let mut v = Vec::new();
    if !core.is_ascii() {
        return v;
    }
    for (pi, (pat, _)) in OCR.iter().enumerate() {
        for (pos, _) in core.match_indices(pat) {
            v.push((pi, pos));
        }
    }
    v
}

fn applicable(op: &str, tok: &str) -> bool {
    let (lead, core, trail) = split_core(tok);
    match op {
        "typo" => core.is_ascii() && core.chars().filter(|c| c.is_ascii_alphabetic()).count() >= 3,
        "ocr" => !ocr_sites(core).is_empty(),
        "synonym" => synonym(core).is_some(),
        "article_drop" => lead.is_empty() && trail.is_empty() && ARTICLES.contains(&core.to_ascii_lowercase().as_str()),
        "word_drop" => !core.is_empty() && lead.is_empty() && !trail.contains(['.', '!', '?', ':', ';']),
        "apostrophe" => apostrophe_at(core).is_some(),
        "casing" => core.chars().next().is_some_and(|c| c.is_ascii_alphabetic()),
        "punct" => trail.ends_with([',', ';', ':']),
        "zero_width" => tok.chars().count() >= 2,
        "homoglyph" => core.chars().any(|c| HOMOGLYPHS.iter().any(|(a, _)| *a == c)),
        _ => false,
    }
}

/// Apply a word-level operation; returns the new word and a variant label.
fn apply(op: &str, tok: &str, r: &mut Rng) -> (String, String) {
    let (lead, core, trail) = split_core(tok);
    let wrap = |c: String| format!("{lead}{c}{trail}");
    match op {
        "typo" => {
            let mut cs: Vec<char> = core.chars().collect();
            let n = cs.len();
            let kind = r.below(4);
            let orig = cs.clone();
            let variant = match kind {
                0 if n >= 3 => {
                    let i = 1 + r.below(n - 2);
                    cs.swap(i, i + 1);
                    "transpose"
                }
                1 => {
                    let i = 1 + r.below(n - 1);
                    cs.remove(i);
                    "drop_letter"
                }
                3 => {
                    let i = 1 + r.below(n - 1);
                    let nb = neighbours(cs[i]);
                    if nb.is_empty() {
                        cs.insert(i, cs[i]);
                        "double_letter"
                    } else {
                        cs[i] = nb[r.below(nb.len())];
                        "keyboard_neighbour"
                    }
                }
                _ => {
                    let i = r.below(n);
                    cs.insert(i, cs[i]);
                    "double_letter"
                }
            };
            if cs == orig {
                let i = r.below(n);
                let c = orig[i];
                let mut d = orig.clone();
                d.insert(i, c);
                return (wrap(d.into_iter().collect()), "double_letter".into());
            }
            (wrap(cs.into_iter().collect()), variant.into())
        }
        "ocr" => {
            let sites = ocr_sites(core);
            let (pi, pos) = sites[r.below(sites.len())];
            let (pat, rep) = OCR[pi];
            (wrap(format!("{}{}{}", &core[..pos], rep, &core[pos + pat.len()..])), format!("{pat}->{rep}"))
        }
        "synonym" => (wrap(synonym(core).unwrap()), "word_list_v1".into()),
        "apostrophe" => {
            let i = apostrophe_at(core).unwrap();
            let c = core[i..].chars().next().unwrap();
            (wrap(format!("{}{}", &core[..i], &core[i + c.len_utf8()..])), "drop_apostrophe".into())
        }
        "casing" => {
            let first = core.chars().next().unwrap();
            let mut opts: Vec<&str> = Vec::new();
            if first.is_ascii_uppercase() {
                opts.push("lower_initial");
            } else {
                opts.push("upper_initial");
                if core.len() >= 4 && core.chars().all(|c| c.is_ascii_lowercase()) {
                    opts.push("all_caps");
                }
            }
            let v = opts[r.below(opts.len())];
            let c = match v {
                "lower_initial" => format!("{}{}", first.to_ascii_lowercase(), &core[1..]),
                "upper_initial" => format!("{}{}", first.to_ascii_uppercase(), &core[1..]),
                _ => core.to_ascii_uppercase(),
            };
            (wrap(c), v.into())
        }
        "punct" => {
            let t = &trail[..trail.len() - 1];
            (format!("{lead}{core}{t}"), format!("drop_{}", &trail[trail.len() - 1..]))
        }
        "zero_width" => {
            let idx: Vec<usize> = tok.char_indices().map(|(i, _)| i).collect();
            let at = idx[1 + r.below(idx.len() - 1)];
            let z = ZERO_WIDTH[r.below(ZERO_WIDTH.len())];
            (format!("{}{}{}", &tok[..at], z, &tok[at..]), format!("U+{:04X}", z as u32))
        }
        "homoglyph" => {
            let sites: Vec<(usize, char, char)> =
                core.char_indices().filter_map(|(i, c)| HOMOGLYPHS.iter().find(|(a, _)| *a == c).map(|(a, b)| (i, *a, *b))).collect();
            let (i, a, b) = sites[r.below(sites.len())];
            (wrap(format!("{}{}{}", &core[..i], b, &core[i + a.len_utf8()..])), format!("{a}->U+{:04X}", b as u32))
        }
        _ => unreachable!("op {op}"),
    }
}

/// One augmentation result.
#[derive(Clone, Debug)]
pub struct Augmented {
    pub text: String,
    /// Model-written byte spans of `text` (sorted, disjoint).
    pub spans: Vec<(usize, usize)>,
    pub words: usize,
    pub model_words: usize,
    /// Per-row record (schema `slop_ninja_a_augmentation_v1`), no text.
    pub record: Value,
}

impl Augmented {
    pub fn share(&self) -> f64 {
        if self.words == 0 { 0.0 } else { self.model_words as f64 / self.words as f64 }
    }
}

/// Validate spans against the text and return each word's byte range and
/// label (0 human, 1 model). A span may not cut a word or hold no word.
pub fn word_labels(text: &str, spans: &[(usize, usize)]) -> Result<Vec<((usize, usize), u8)>> {
    let mut prev = 0;
    for &(a, b) in spans {
        ensure!(a >= prev && a < b && b <= text.len(), "spans must be sorted, disjoint, non-empty and inside the text");
        ensure!(text.is_char_boundary(a) && text.is_char_boundary(b), "span off a character boundary");
        prev = b;
    }
    let mut out = Vec::new();
    let mut pos = 0;
    for w in text.split_whitespace() {
        let st = pos + text[pos..].find(w).unwrap();
        let en = st + w.len();
        pos = en;
        ensure!(!spans.iter().any(|&(a, b)| (a > st && a < en) || (b > st && b < en)), "a span cuts the word at bytes {st}..{en}");
        out.push(((st, en), spans.iter().any(|&(a, b)| a <= st && en <= b) as u8));
    }
    for &(a, b) in spans {
        ensure!(out.iter().any(|&((s, e), _)| a <= s && e <= b), "span {a}..{b} holds no word");
    }
    Ok(out)
}

/// Noise `text` under `profile`. `spans` are the model-written byte spans
/// (empty for a human text). Deterministic in (profile, seed, row_key, text).
pub fn augment(text: &str, spans: &[(usize, usize)], profile: &Profile, seed: u64, row_key: &str) -> Result<Augmented> {
    profile.validate()?;
    let words = word_labels(text, spans)?;
    let n = words.len();
    let seed_s = seed.to_string();
    let base = [KEY_PREFIX, NOISE_MODEL, profile.name.as_str(), seed_s.as_str(), row_key];
    let mut row_rng = Rng::sub(&base, &["row"]);
    let intensity = profile.intensity_min + row_rng.unit() * (profile.intensity_max - profile.intensity_min);
    let cap = ((profile.max_edit_fraction * n as f64).floor() as usize).max(usize::from(n > 0 && profile.max_edit_fraction > 0.0));
    // labelled runs for the deletion guard
    let mut run_of = vec![0usize; n];
    let mut run_left: Vec<usize> = Vec::new();
    for k in 0..n {
        if k == 0 || words[k].1 != words[k - 1].1 {
            run_left.push(0);
        }
        run_of[k] = run_left.len() - 1;
        *run_left.last_mut().unwrap() += 1;
    }
    let mut toks: Vec<String> = words.iter().map(|&((s, e), _)| text[s..e].to_owned()).collect();
    let mut deleted = vec![false; n];
    let mut edits: Vec<Value> = Vec::new();
    let mut counts: BTreeMap<String, [usize; 2]> = BTreeMap::new();
    let mut guard_skips = 0usize;
    let mut capped = false;
    let mut tok_rng = Rng::sub(&base, &["tokens"]);
    let mut word_edits = 0usize;
    for k in 0..n {
        let u = tok_rng.unit();
        let probs: Vec<f64> =
            TOKEN_OPS.iter().map(|o| if applicable(o, &toks[k]) { profile.rate(o) / 100.0 * intensity } else { 0.0 }).collect();
        let total: f64 = probs.iter().sum();
        if u >= total {
            continue;
        }
        if word_edits >= cap {
            capped = true;
            continue;
        }
        let mut acc = 0.0;
        let mut op = TOKEN_OPS[TOKEN_OPS.len() - 1];
        for (o, p) in TOKEN_OPS.iter().zip(&probs) {
            acc += p;
            if u < acc {
                op = o;
                break;
            }
        }
        let label = words[k].1;
        if matches!(op, "article_drop" | "word_drop") {
            if run_left[run_of[k]] <= 1 {
                guard_skips += 1;
                continue;
            }
            run_left[run_of[k]] -= 1;
            deleted[k] = true;
            word_edits += 1;
            counts.entry(op.into()).or_default()[label as usize] += 1;
            edits.push(json!([k, op, "delete", label]));
            continue;
        }
        let ks = k.to_string();
        let mut r = Rng::sub(&base, &["token", ks.as_str()]);
        let (new, variant) = apply(op, &toks[k], &mut r);
        debug_assert!(!new.chars().any(char::is_whitespace));
        toks[k] = new;
        word_edits += 1;
        counts.entry(op.into()).or_default()[label as usize] += 1;
        edits.push(json!([k, op, variant, label]));
    }
    // forced edits: guarantee min(min_edits, cap) word edits when possible
    let mut forced = 0usize;
    let mut edited = vec![false; n];
    for e in &edits {
        edited[e[0].as_u64().unwrap() as usize] = true;
    }
    let mut fr = Rng::sub(&base, &["forced"]);
    while word_edits < profile.min_edits.min(cap) {
        let cands: Vec<(usize, Vec<(&str, f64)>)> = (0..n)
            .filter(|&k| !deleted[k] && !edited[k])
            .filter_map(|k| {
                let ops: Vec<(&str, f64)> = TOKEN_OPS
                    .iter()
                    .filter(|o| !matches!(**o, "article_drop" | "word_drop") && profile.rate(o) > 0.0 && applicable(o, &toks[k]))
                    .map(|o| (*o, profile.rate(o)))
                    .collect();
                (!ops.is_empty()).then_some((k, ops))
            })
            .collect();
        if cands.is_empty() {
            break;
        }
        let (k, ops) = &cands[fr.below(cands.len())];
        let u = fr.unit() * ops.iter().map(|x| x.1).sum::<f64>();
        let mut acc = 0.0;
        let mut op = ops[ops.len() - 1].0;
        for (o, w) in ops {
            acc += w;
            if u < acc {
                op = o;
                break;
            }
        }
        let ks = k.to_string();
        let mut r = Rng::sub(&base, &["forced_token", ks.as_str()]);
        let (new, variant) = apply(op, &toks[*k], &mut r);
        toks[*k] = new;
        edited[*k] = true;
        word_edits += 1;
        forced += 1;
        let label = words[*k].1;
        counts.entry(op.into()).or_default()[label as usize] += 1;
        edits.push(json!([k, op, variant, label]));
    }
    // gaps: lead, the separator before each kept word, trailing
    let lead = &text[..words.first().map(|w| w.0.0).unwrap_or(text.len())];
    let trailing = &text[words.last().map(|w| w.0.1).unwrap_or(text.len())..];
    let kept: Vec<usize> = (0..n).filter(|&k| !deleted[k]).collect();
    let gap_cap = cap;
    let mut gap_edits = 0usize;
    let mut out = String::from(lead);
    let mut out_words: Vec<((usize, usize), u8)> = Vec::new();
    for (i, &k) in kept.iter().enumerate() {
        if i > 0 {
            // separator: the original gap immediately before word k
            let g = &text[words[k - 1].0.1..words[k].0.0];
            let ks = k.to_string();
            let mut r = Rng::sub(&base, &["gap", ks.as_str()]);
            let u = r.unit();
            let p = profile.rate(GAP_OP) / 100.0 * intensity;
            let mut sep = g.to_owned();
            if u < p {
                if gap_edits >= gap_cap {
                    capped = true;
                } else {
                    let opts: Vec<&str> = if g == " " {
                        vec!["double_space", "nbsp", "newline"]
                    } else if g.contains('\n') {
                        vec!["newline_to_space", "extra_newline"]
                    } else {
                        vec!["widen"]
                    };
                    let v = opts[r.below(opts.len())];
                    sep = match v {
                        "double_space" => "  ".into(),
                        "nbsp" => "\u{a0}".into(),
                        "newline" => "\n".into(),
                        "newline_to_space" => " ".into(),
                        "extra_newline" => format!("{g}\n"),
                        _ => format!("{g} "),
                    };
                    gap_edits += 1;
                    let inside = (words[kept[i - 1]].1 == 1 && words[k].1 == 1) as usize;
                    counts.entry(GAP_OP.into()).or_default()[inside] += 1;
                    edits.push(json!([k, GAP_OP, v, inside]));
                }
            }
            out.push_str(&sep);
        }
        let s = out.len();
        out.push_str(&toks[k]);
        out_words.push(((s, out.len()), words[k].1));
    }
    out.push_str(trailing);
    // remap spans: runs of model words; a span that covered the leading or
    // trailing whitespace keeps covering it
    let covers_start = spans.first().is_some_and(|s| s.0 == 0);
    let covers_end = spans.last().is_some_and(|s| s.1 == text.len());
    let mut new_spans: Vec<(usize, usize)> = Vec::new();
    for (i, &((s, e), l)) in out_words.iter().enumerate() {
        if l == 0 {
            continue;
        }
        if i > 0 && out_words[i - 1].1 == 1 {
            new_spans.last_mut().unwrap().1 = e;
        } else {
            new_spans.push((s, e));
        }
    }
    if covers_start && out_words.first().is_some_and(|w| w.1 == 1) {
        new_spans[0].0 = 0;
    }
    if covers_end && out_words.last().is_some_and(|w| w.1 == 1) {
        new_spans.last_mut().unwrap().1 = out.len();
    }
    // exactness check: recomputed labels equal the carried labels
    let check = word_labels(&out, &new_spans)?;
    ensure!(check.len() == out_words.len(), "word count changed outside deletions");
    ensure!(check.iter().zip(&out_words).all(|(a, b)| a.1 == b.1), "label remap mismatch");
    let model_words = out_words.iter().filter(|w| w.1 == 1).count();
    let src_model = words.iter().filter(|w| w.1 == 1).count();
    let by_op: Map<String, Value> =
        counts.iter().map(|(k, v)| (k.clone(), json!({"human": v[0], "model": v[1]}))).collect();
    let record = json!({
        "schema": RECORD_SCHEMA,
        "noise_model": NOISE_MODEL,
        "profile": profile.name,
        "profile_sha256": profile.sha256(),
        "seed": seed,
        "row_key": row_key,
        "intensity": intensity,
        "word_policy": WORD_POLICY,
        "source_text_sha256": hex::encode(Sha256::digest(text.as_bytes())),
        "text_sha256": hex::encode(Sha256::digest(out.as_bytes())),
        "words_before": n,
        "words_after": out_words.len(),
        "model_words_before": src_model,
        "model_words_after": model_words,
        "word_edits": word_edits,
        "gap_edits": gap_edits,
        "edits_by_operation": by_op,
        "deletions_skipped_by_run_guard": guard_skips,
        "forced_edits": forced,
        "edit_cap": cap,
        "edit_cap_reached": capped,
        "edits_schema": "[source word index, operation, variant, class: 0 human / 1 model (whitespace: 1 when inside a model span)]",
        "edits": edits,
        "label_blind_draws": true,
    });
    Ok(Augmented { text: out, spans: new_spans, words: out_words.len(), model_words, record })
}

// ------------------------------------------------------------ bundle rows

const SPAN_KEYS: [&str; 3] = ["ai_byte_spans", "spans", "replacement_spans"];
const SHARE_KEYS: [&str; 2] = ["share", "measured_share"];
const ORIGIN_KEYS: [&str; 2] = ["origin", "origin_class"];

fn first_key<'a>(row: &'a Value, keys: &[&'static str]) -> Option<(&'static str, &'a Value)> {
    keys.iter().find_map(|k| row.get(*k).filter(|v| !v.is_null()).map(|v| (*k, v)))
}

/// (row id, origin, model-written byte spans) of a checked bundle row.
pub type RowParts<'a> = (String, &'a str, Vec<(usize, usize)>);

/// Read a bundle row's text and model spans and check them against its
/// digest, origin and share.
pub fn row_parts(row: &Value) -> Result<RowParts<'_>> {
    let id = row["id"].as_str().context("row id")?;
    let text = row["text"].as_str().with_context(|| format!("row {id} has no text"))?;
    if let Some(d) = row["text_sha256"].as_str() {
        ensure!(hex::encode(Sha256::digest(text.as_bytes())) == d, "row {id}: text digest mismatch");
    }
    let spans: Vec<(usize, usize)> = match first_key(row, &SPAN_KEYS) {
        Some((_, v)) => v
            .as_array()
            .context("spans array")?
            .iter()
            .map(|p| Ok((p[0].as_u64().context("span start")? as usize, p[1].as_u64().context("span end")? as usize)))
            .collect::<Result<_>>()?,
        None => vec![],
    };
    let labels = word_labels(text, &spans).with_context(|| format!("row {id}"))?;
    let m = labels.iter().filter(|w| w.1 == 1).count();
    let origin = first_key(row, &ORIGIN_KEYS).and_then(|(_, v)| v.as_str()).with_context(|| format!("row {id}: origin"))?;
    let want = match origin {
        "human_only" => m == 0,
        "model_only" => m == labels.len() && m > 0,
        "mixed" => m > 0 && m < labels.len(),
        o => bail!("row {id}: unknown origin {o}"),
    };
    ensure!(want, "row {id}: origin {origin} disagrees with its spans ({m} of {} words model-written)", labels.len());
    if let Some((_, s)) = first_key(row, &SHARE_KEYS) {
        let s = s.as_f64().context("share")?;
        let got = if labels.is_empty() { 0.0 } else { m as f64 / labels.len() as f64 };
        ensure!((s - got).abs() <= 1e-9, "row {id}: share {s} disagrees with its spans ({got})");
    }
    Ok((id.to_owned(), origin, spans))
}

/// The row with `a`'s text, digest, spans and share (and `word_count` when
/// present) under the row's own field names, `a`'s record under
/// `record_key` and a new id; every other field is kept.
fn rebuild(row: &Value, new_id: String, origin: &str, a: &Augmented, record_key: &str) -> Result<Value> {
    let mut out = row.clone();
    let o = out.as_object_mut().context("row object")?;
    o.insert("id".into(), json!(new_id));
    o.insert("text".into(), json!(a.text));
    o.insert("text_sha256".into(), a.record["text_sha256"].clone());
    let sk = first_key(row, &SPAN_KEYS).map(|x| x.0).unwrap_or("ai_byte_spans");
    o.insert(sk.into(), json!(a.spans.iter().map(|&(s, e)| [s, e]).collect::<Vec<_>>()));
    let share = if origin == "model_only" { 1.0 } else { a.share() };
    let shk = first_key(row, &SHARE_KEYS).map(|x| x.0).unwrap_or("share");
    o.insert(shk.into(), json!(share));
    if o.contains_key("word_count") {
        o.insert("word_count".into(), json!(a.words));
    }
    o.insert(record_key.into(), a.record.clone());
    Ok(out)
}

/// An augmented copy of a bundle row: same split, source group, writer,
/// labels and metadata; new id (`<id>#aug:<profile>:<seed>`), text, digest,
/// spans, share (and `word_count` when present), plus `augmentation`,
/// `augmented_from` and `augmented_from_text_sha256`.
pub fn augment_row(row: &Value, profile: &Profile, seed: u64) -> Result<Value> {
    let (id, origin, spans) = row_parts(row)?;
    let a = augment(row["text"].as_str().unwrap(), &spans, profile, seed, &id)?;
    let mut out = rebuild(row, format!("{id}#aug:{}:{seed}", profile.name), origin, &a, "augmentation")?;
    out["augmented_from"] = json!(id);
    out["augmented_from_text_sha256"] = a.record["source_text_sha256"].clone();
    Ok(out)
}

/// Per-class accounting of a batch of augmentations (symmetry audit).
#[derive(Default)]
pub struct Audit {
    rows: BTreeMap<String, [usize; 2]>,
    words: [usize; 2],
    edits: BTreeMap<String, [usize; 2]>,
    guard_skips: usize,
}

impl Audit {
    /// Count one source row (selected or not) and, when augmented, its record.
    pub fn add(&mut self, origin: &str, words: (usize, usize), rec: Option<&Value>) {
        let e = self.rows.entry(origin.into()).or_default();
        e[0] += 1;
        if let Some(r) = rec {
            e[1] += 1;
            self.words[0] += words.0;
            self.words[1] += words.1;
            if let Some(m) = r["edits_by_operation"].as_object() {
                for (k, v) in m {
                    let c = self.edits.entry(k.clone()).or_default();
                    c[0] += v["human"].as_u64().unwrap_or(0) as usize;
                    c[1] += v["model"].as_u64().unwrap_or(0) as usize;
                }
            }
            self.guard_skips += r["deletions_skipped_by_run_guard"].as_u64().unwrap_or(0) as usize;
        }
    }
    pub fn report(&self) -> Value {
        let per100 = |c: usize, w: usize| if w == 0 { Value::Null } else { json!(100.0 * c as f64 / w as f64) };
        let rows: Map<String, Value> = self
            .rows
            .iter()
            .map(|(k, v)| (k.clone(), json!({"rows": v[0], "augmented": v[1], "rate": if v[0] == 0 { Value::Null } else { json!(v[1] as f64 / v[0] as f64) }})))
            .collect();
        let edits: Map<String, Value> = self
            .edits
            .iter()
            .map(|(k, v)| {
                (k.clone(), json!({"human_words_edited": v[0], "model_words_edited": v[1],
                    "per_100_human_words": per100(v[0], self.words[0]), "per_100_model_words": per100(v[1], self.words[1])}))
            })
            .collect();
        json!({"rows_by_origin": rows, "augmented_human_words": self.words[0], "augmented_model_words": self.words[1],
            "edits_by_operation": edits, "deletions_skipped_by_run_guard": self.guard_skips,
            "note": "whitespace counts gaps: human = outside or on the edge of a model span, model = inside one"})
    }
}

/// Word counts (human, model) of a row's text and spans.
pub fn class_words(text: &str, spans: &[(usize, usize)]) -> Result<(usize, usize)> {
    let w = word_labels(text, spans)?;
    let m = w.iter().filter(|x| x.1 == 1).count();
    Ok((w.len() - m, m))
}

/// Consistency probes for one row: an exact repeat when selected at
/// `repeat_rate` and a light perturbation when selected at `perturb_rate`
/// (independent class-blind draws). Returns (manifest line, probe row) pairs.
pub fn probes_for_row(row: &Value, profile: &Profile, seed: u64, repeat_rate: f64, perturb_rate: f64) -> Result<Vec<(Value, Value)>> {
    let (id, origin, spans) = row_parts(row)?;
    let text = row["text"].as_str().unwrap();
    let base_sha = hex::encode(Sha256::digest(text.as_bytes()));
    let split = row["split"].clone();
    let mut out = Vec::new();
    if selected("probe_repeat", seed, &id, repeat_rate) {
        let pid = format!("probe:repeat:{seed}:{id}");
        let mut pr = row.clone();
        pr["id"] = json!(pid);
        pr["probe_of"] = json!(id);
        let m = json!({"schema": PROBE_SCHEMA, "probe_id": pid, "base_id": id, "split": split, "kind": "repeat", "origin": origin,
            "base_text_sha256": base_sha, "text_sha256": base_sha, "noise_model": null, "profile": null, "seed": seed,
            "edits_by_operation": {}, "word_edits": 0, "gap_edits": 0});
        out.push((m, pr));
    }
    if selected("probe_perturb", seed, &id, perturb_rate) {
        // probe-keyed draws, distinct from the training-copy draws
        let a = augment(text, &spans, profile, seed, &format!("probe:{id}"))?;
        let pid = format!("probe:perturbed:{seed}:{id}");
        let mut pr = rebuild(row, pid.clone(), origin, &a, "augmentation")?;
        pr["probe_of"] = json!(id);
        let m = json!({"schema": PROBE_SCHEMA, "probe_id": pid, "base_id": id, "split": split, "kind": "perturbed", "origin": origin,
            "base_text_sha256": base_sha, "text_sha256": a.record["text_sha256"], "noise_model": NOISE_MODEL, "profile": profile.name,
            "profile_sha256": profile.sha256(), "seed": seed, "edits_by_operation": a.record["edits_by_operation"],
            "word_edits": a.record["word_edits"], "gap_edits": a.record["gap_edits"],
            "text_changed": a.record["text_sha256"] != json!(base_sha)});
        out.push((m, pr));
    }
    Ok(out)
}

// ------------------------------------------------------------ form injection

/// Human injection twins (`gen_form_inject_v1`, protocol A2 §7.4, H-inj)
/// and the per-class audit that fixes the variants they apply (S0-13).
///
/// * **Groups.** H-inj-C injects conventions: quotes, dashes and ellipses
///   rendered in the class variant (`grammar_a_form::conventions`, the
///   normaliser's own sites and renderers), and paired markdown emphasis on
///   one phrase. H-inj-L injects whitespace layout: the class variant as the
///   trailing whitespace of a keyed half of the content lines, and the
///   blank-line separator variant at every paragraph boundary
///   (`grammar_a_form::measure`). Each twin applies its group's class
///   variants together.
/// * **Line endings.** A `'\r'` run before a newline is a line ending, not
///   layout: it is not trailing whitespace, separators are measured with it
///   removed, and a separator written after a CRLF line is written with CRLF
///   endings.
/// * **Variants** (S0-13). Per class, the variant whose document-level
///   prevalence most exceeds in raw Train-2 positives what it is in Train-2
///   humans: a document shows a variant when it holds at least one
///   occurrence, and the score is `p_v/P - h_v/H`. Separator variants are
///   blank-line runs only (gaps of two or more newlines). The largest
///   positive score wins; ties go to the larger positive count, then to the
///   class's fixed order (enumerated classes) or the escaped string's byte
///   order (whitespace classes). A class with no positive score has no
///   variant, and its twins inject nothing for it. The audit reads the
///   positive-weight rows of assembler Train split directories only
///   ([`for_train_split`]); the twins read the variants from the frozen
///   S0-13 entry of `VALUES-R2.json` ([`variants_from_values`]).
/// * **Keyed draws** use round-2 plan keys (protocol value V21, unkeyed):
///   the half of lines is the first `ceil(n / 2)` content lines in ascending
///   order of the key with x = `h_inj_l_line` and d.id = `<row key>:<line
///   index>`; the phrase is the candidate whose first word has the smallest
///   key with x = `h_inj_c_phrase` and d.id = `<row key>:<word index>`.
///   Labels never enter a draw.
/// * **Exactness.** Every edit replaces or inserts form characters only:
///   the sequence of alphanumeric runs is unchanged (H-inj-L also keeps the
///   whitespace-word sequence byte for byte). Model spans are remapped
///   exactly and the remapped labels are re-checked run by run. The one
///   label-aware rule is a guard: a dash replacement that joins two words
///   (a spaced dash rendered closed) is skipped, and counted, when a span
///   boundary lies on it, so a word never straddles a span edge.
///
/// Records, manifests and audits hold counts, variant names, escaped
/// whitespace strings and digests, never text.
pub mod form_inject {
    use crate::{Augmented, rebuild, row_parts, word_labels};
    use anyhow::{Context, Result, bail, ensure};
    use grammar_a_form::conventions::{self, Changes, Choice, Conventions, DASHES, Dash, Ellipsis, Quote, Site};
    use grammar_a_form::measure::{GapKind, Measure, escape_ws};
    use grammar_a_form::profile::R2_PLAN_KEY_PREFIX;
    use serde_json::{Map, Value, json};
    use sha2::{Digest, Sha256};
    use std::collections::{BTreeMap, BTreeSet};
    use std::fs;
    use std::io::{BufRead, BufReader, Read};
    use std::path::{Path, PathBuf};

    /// The profile id (`slop_ninja_a_augment probes --profile gen_form_inject_v1`).
    pub const PROFILE: &str = "gen_form_inject_v1";
    /// Per-twin record (the twin row's `form_inject` field).
    pub const RECORD_SCHEMA: &str = "slop_ninja_a_form_inject_v1";
    /// Text-free manifest line of one twin.
    pub const PROBE_SCHEMA: &str = "slop_ninja_a_form_inject_probe_v1";
    /// The per-class audit (S0-13).
    pub const AUDIT_SCHEMA: &str = "slop_ninja_a_form_inject_audit_v1";
    /// V21 key name of the keyed half of lines (H-inj-L).
    pub const LINE_KEY: &str = "h_inj_l_line";
    /// V21 key name of the emphasised phrase (H-inj-C).
    pub const PHRASE_KEY: &str = "h_inj_c_phrase";
    /// Paired markdown emphasis markers, as `layout::strip_emphasis` pairs them.
    pub const MARKERS: [&str; 2] = ["**", "__"];
    /// The audit's statistic, as recorded.
    pub const STATISTIC: &str = "per class and variant: p_v = positives showing v (at least one occurrence), h_v = humans showing v; unweighted documents, mixed positives whole; separator variants are blank-line runs only (gaps of two or more newlines, '\\r' before a newline removed); score = p_v/P - h_v/H, compared exactly as p_v*H - h_v*P; the largest positive score wins; ties: larger p_v, then the class's fixed order (quote, dash, ellipsis, emphasis) or the escaped string's byte order (trailing_whitespace, separator); no positive score: no variant";
    /// Which rows the audit reads, as recorded.
    pub const INPUT_RULE: &str = "assembler Train split directories only (MANIFEST slop_ninja_a_bundle_manifest_v1, split train, not sealed, rows.jsonl digest and row count as the manifest says; the read stops at the first row that is not a Train row); humans = human_only rows with positive weight of the --humans splits; positives = mixed and model_only rows with positive weight of the --positives splits; zero-weight rows count for nothing; a row id at most once per role";
    /// The assembler's split manifest schema (`grammar-a-assemble`'s
    /// `bundle::MANIFEST_SCHEMA`).
    pub const TRAIN_MANIFEST_SCHEMA: &str = "slop_ninja_a_bundle_manifest_v1";
    /// The round-2 value register's schema (`VALUES-R2.json`).
    pub const VALUES_SCHEMA: &str = "slop_ninja_a_round2_values_v1";
    /// The register key of the frozen variants (protocol A2 §12.6).
    pub const VALUES_KEY: &str = "S0-13";

    // ------------------------------------------------------------ classes

    /// The two twin groups.
    #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
    pub enum Group {
        C,
        L,
    }
    pub const GROUPS: [Group; 2] = [Group::C, Group::L];

    impl Group {
        /// The twin kind (manifest `kind`, probe id infix).
        pub fn kind(self) -> &'static str {
            match self {
                Group::C => "h_inj_c",
                Group::L => "h_inj_l",
            }
        }
        pub fn classes(self) -> &'static [Class] {
            match self {
                Group::C => &CLASSES[..4],
                Group::L => &CLASSES[4..],
            }
        }
    }

    /// The injection classes.
    #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
    pub enum Class {
        Quote,
        Dash,
        Ellipsis,
        Emphasis,
        TrailingWhitespace,
        Separator,
    }
    pub const CLASSES: [Class; 6] = [Class::Quote, Class::Dash, Class::Ellipsis, Class::Emphasis, Class::TrailingWhitespace, Class::Separator];

    impl Class {
        pub fn name(self) -> &'static str {
            match self {
                Class::Quote => "quote",
                Class::Dash => "dash",
                Class::Ellipsis => "ellipsis",
                Class::Emphasis => "emphasis",
                Class::TrailingWhitespace => "trailing_whitespace",
                Class::Separator => "separator",
            }
        }
        pub fn group(self) -> Group {
            match self {
                Class::TrailingWhitespace | Class::Separator => Group::L,
                _ => Group::C,
            }
        }
        /// The fixed variant order of an enumerated class; `None` for the
        /// whitespace classes, whose variants are escaped strings.
        pub fn enumerated(self) -> Option<Vec<&'static str>> {
            match self {
                Class::Quote => Some(vec!["curly", "ascii"]),
                Class::Dash => Some(DASHES.iter().map(|d| dash_name(*d)).collect()),
                Class::Ellipsis => Some(vec!["char", "dots"]),
                Class::Emphasis => Some(MARKERS.to_vec()),
                Class::TrailingWhitespace | Class::Separator => None,
            }
        }
    }

    /// A dash style's variant name (the `conventions` serde name).
    pub fn dash_name(d: Dash) -> &'static str {
        match d {
            Dash::EmClosed => "em_closed",
            Dash::EmSpaced => "em_spaced",
            Dash::EnSpaced => "en_spaced",
            Dash::HyphenSpaced => "hyphen_spaced",
            Dash::DoubleClosed => "double_closed",
            Dash::DoubleSpaced => "double_spaced",
        }
    }

    fn quote_name(q: Quote) -> &'static str {
        match q {
            Quote::Curly => "curly",
            Quote::Ascii => "ascii",
        }
    }

    fn ellipsis_name(e: Ellipsis) -> &'static str {
        match e {
            Ellipsis::Char => "char",
            Ellipsis::Dots => "dots",
        }
    }

    /// The inverse of `measure::escape_ws`: `\n`, `\r`, `\t`, `\s` and
    /// `\u{XXXX}`, refusing anything that is not the canonical escape of a
    /// whitespace string.
    pub fn unescape_ws(s: &str) -> Result<String> {
        let mut out = String::new();
        let mut rest = s;
        while !rest.is_empty() {
            let r = rest.strip_prefix('\\').with_context(|| format!("escaped whitespace {s:?}: expected a backslash escape"))?;
            let (c, used) = match r.chars().next() {
                Some('n') => ('\n', 1),
                Some('r') => ('\r', 1),
                Some('t') => ('\t', 1),
                Some('s') => (' ', 1),
                Some('u') => {
                    let body = r[1..].strip_prefix('{').with_context(|| format!("escaped whitespace {s:?}: \\u needs braces"))?;
                    let end = body.find('}').with_context(|| format!("escaped whitespace {s:?}: unclosed \\u{{"))?;
                    let n = u32::from_str_radix(&body[..end], 16).with_context(|| format!("escaped whitespace {s:?}: hex"))?;
                    (char::from_u32(n).with_context(|| format!("escaped whitespace {s:?}: not a scalar value"))?, 2 + end + 1)
                }
                _ => bail!("escaped whitespace {s:?}: unknown escape"),
            };
            out.push(c);
            rest = &r[used..];
        }
        ensure!(!out.is_empty() && out.chars().all(char::is_whitespace), "escaped whitespace {s:?} is empty or not whitespace");
        ensure!(escape_ws(&out) == s, "escaped whitespace {s:?} is not in canonical form ({})", escape_ws(&out));
        Ok(out)
    }

    /// A line's trailing-whitespace variant: its trailing whitespace less
    /// the `'\r'` run that ends it (a CRLF line ending is not trailing
    /// whitespace).
    fn trailing_variant(t: &str) -> &str {
        t.trim_end_matches('\r')
    }

    /// `s` with every `'\r'` run that stands right before a newline removed
    /// (CRLF line endings read as LF). Idempotent.
    pub fn lf(s: &str) -> String {
        let mut out = String::with_capacity(s.len());
        let mut cr = 0usize;
        for c in s.chars() {
            match c {
                '\r' => cr += 1,
                '\n' => {
                    cr = 0;
                    out.push('\n');
                }
                c => {
                    out.extend(std::iter::repeat_n('\r', cr));
                    cr = 0;
                    out.push(c);
                }
            }
        }
        out.extend(std::iter::repeat_n('\r', cr));
        out
    }

    /// The separator variant `s` (LF form) as written after a line: with
    /// CRLF endings (every newline after the first preceded by `'\r'`; the
    /// first newline's `'\r'` is the line's own) when the line ends with
    /// `'\r'`.
    fn separator_after(s: &str, crlf: bool) -> String {
        if !crlf {
            return s.to_string();
        }
        let mut out = String::with_capacity(s.len() + 4);
        for (k, c) in s.chars().enumerate() {
            if c == '\n' && k > 0 {
                out.push('\r');
            }
            out.push(c);
        }
        out
    }

    // ------------------------------------------------------------ variants

    /// The variant each class injects (S0-13); `None` injects nothing for
    /// that class. Whitespace variants are raw strings here and escaped in
    /// JSON.
    #[derive(Clone, Debug, Default, PartialEq)]
    pub struct Variants {
        pub quote: Option<Quote>,
        pub dash: Option<Dash>,
        pub ellipsis: Option<Ellipsis>,
        pub emphasis: Option<&'static str>,
        pub trailing_whitespace: Option<String>,
        pub separator: Option<String>,
    }

    impl Variants {
        /// The variant name of class `c` (escaped for whitespace classes).
        pub fn name(&self, c: Class) -> Option<String> {
            match c {
                Class::Quote => self.quote.map(|q| quote_name(q).to_string()),
                Class::Dash => self.dash.map(|d| dash_name(d).to_string()),
                Class::Ellipsis => self.ellipsis.map(|e| ellipsis_name(e).to_string()),
                Class::Emphasis => self.emphasis.map(str::to_string),
                Class::TrailingWhitespace => self.trailing_whitespace.as_deref().map(escape_ws),
                Class::Separator => self.separator.as_deref().map(escape_ws),
            }
        }

        /// Sets class `c` from its variant name, refusing names the class
        /// cannot inject.
        pub fn set(&mut self, c: Class, name: Option<&str>) -> Result<()> {
            let Some(n) = name else {
                match c {
                    Class::Quote => self.quote = None,
                    Class::Dash => self.dash = None,
                    Class::Ellipsis => self.ellipsis = None,
                    Class::Emphasis => self.emphasis = None,
                    Class::TrailingWhitespace => self.trailing_whitespace = None,
                    Class::Separator => self.separator = None,
                }
                return Ok(());
            };
            let bad = || format!("{}: unknown variant {n:?}", c.name());
            match c {
                Class::Quote => {
                    self.quote = Some(match n {
                        "curly" => Quote::Curly,
                        "ascii" => Quote::Ascii,
                        _ => bail!(bad()),
                    })
                }
                Class::Dash => self.dash = Some(*DASHES.iter().find(|d| dash_name(**d) == n).with_context(bad)?),
                Class::Ellipsis => {
                    self.ellipsis = Some(match n {
                        "char" => Ellipsis::Char,
                        "dots" => Ellipsis::Dots,
                        _ => bail!(bad()),
                    })
                }
                Class::Emphasis => self.emphasis = Some(*MARKERS.iter().find(|m| **m == n).with_context(bad)?),
                Class::TrailingWhitespace => {
                    let s = unescape_ws(n).with_context(bad)?;
                    ensure!(!s.contains('\n'), "trailing_whitespace variant {n:?} holds a newline");
                    ensure!(trailing_variant(&s) == s, "trailing_whitespace variant {n:?} ends with a CRLF carriage return");
                    self.trailing_whitespace = Some(s);
                }
                Class::Separator => {
                    let s = unescape_ws(n).with_context(bad)?;
                    ensure!(s.starts_with('\n') && s.ends_with('\n'), "separator variant {n:?} must start and end with a newline");
                    ensure!(s.matches('\n').count() >= 2, "separator variant {n:?} is not a blank-line run (two or more newlines)");
                    ensure!(lf(&s) == s, "separator variant {n:?} holds a CRLF line ending (variants are in LF form)");
                    self.separator = Some(s);
                }
            }
            Ok(())
        }

        /// `{class: name or null}` for the six classes.
        pub fn to_json(&self) -> Value {
            Value::Object(CLASSES.iter().map(|c| (c.name().to_string(), json!(self.name(*c)))).collect())
        }

        /// `{class: name or null}` for one group's classes.
        pub fn group_json(&self, g: Group) -> Value {
            Value::Object(g.classes().iter().map(|c| (c.name().to_string(), json!(self.name(*c)))).collect())
        }

        /// Reads `{class: name or null}`: exactly the six classes.
        pub fn from_json(v: &Value) -> Result<Variants> {
            let o = v.as_object().context("variants must be an object")?;
            let want: BTreeSet<&str> = CLASSES.iter().map(|c| c.name()).collect();
            let got: BTreeSet<&str> = o.keys().map(String::as_str).collect();
            ensure!(got == want, "variants must name exactly the classes {want:?} (got {got:?})");
            let mut out = Variants::default();
            for c in CLASSES {
                let x = &o[c.name()];
                ensure!(x.is_null() || x.is_string(), "{}: variant must be a string or null", c.name());
                out.set(c, x.as_str())?;
            }
            Ok(out)
        }

        fn any(&self, g: Group) -> bool {
            g.classes().iter().any(|c| self.name(*c).is_some())
        }
    }

    // ------------------------------------------------------------ audit

    /// The variants a text shows, per class: those with at least one
    /// occurrence (quote glyphs, double and single alike; `conventions` dash
    /// and ellipsis sites; `**`/`__` pairs on one line; content-line
    /// trailing whitespace less its final `'\r'` run; blank-line runs by
    /// `measure`, in LF form). Single breaks (sentence breaks, line breaks
    /// and hard wraps) show no separator variant.
    pub fn shown(text: &str) -> BTreeMap<Class, BTreeSet<String>> {
        let mut out: BTreeMap<Class, BTreeSet<String>> = CLASSES.iter().map(|c| (*c, BTreeSet::new())).collect();
        let c = conventions::counts(text);
        let mut put = |k: Class, v: String| {
            out.get_mut(&k).expect("class").insert(v);
        };
        if c.dq_curly + c.sq_curly > 0 {
            put(Class::Quote, "curly".into());
        }
        if c.dq_ascii + c.sq_ascii > 0 {
            put(Class::Quote, "ascii".into());
        }
        for (d, n) in &c.dash {
            if *n > 0 {
                put(Class::Dash, dash_name(*d).into());
            }
        }
        for (e, n) in &c.ellipsis {
            if *n > 0 {
                put(Class::Ellipsis, ellipsis_name(*e).into());
            }
        }
        for line in text.split('\n') {
            for m in MARKERS {
                if line.matches(m).count() >= 2 {
                    put(Class::Emphasis, m.into());
                }
            }
        }
        let m = Measure::of(text);
        for i in 0..m.lines.len() {
            let t = trailing_variant(m.trail(i));
            if !t.is_empty() {
                put(Class::TrailingWhitespace, escape_ws(t));
            }
        }
        for (i, g) in m.gaps.iter().enumerate() {
            if g.kind == GapKind::BlankRun {
                put(Class::Separator, escape_ws(&lf(m.separator(i))));
            }
        }
        out
    }

    /// Document counts of every shown variant in positives and humans.
    #[derive(Clone, Debug, Default)]
    pub struct InjectAudit {
        positives: usize,
        humans: usize,
        /// (class, variant) -> [positives showing, humans showing]
        shows: BTreeMap<(Class, String), [usize; 2]>,
    }

    impl InjectAudit {
        /// Counts one document (`positive`: a raw Train-2 positive; else a
        /// Train-2 human).
        pub fn add(&mut self, positive: bool, text: &str) {
            let k = usize::from(!positive);
            if positive {
                self.positives += 1;
            } else {
                self.humans += 1;
            }
            for (c, vs) in shown(text) {
                for v in vs {
                    self.shows.entry((c, v)).or_default()[k] += 1;
                }
            }
        }

        fn order_key(c: Class, v: &str) -> (usize, String) {
            match c.enumerated() {
                Some(names) => (names.iter().position(|n| *n == v).unwrap_or(usize::MAX), String::new()),
                None => (0, v.to_string()),
            }
        }

        /// The chosen variant of each class (see `STATISTIC`).
        pub fn variants(&self) -> Result<Variants> {
            ensure!(self.positives > 0 && self.humans > 0, "the audit needs positives ({}) and humans ({})", self.positives, self.humans);
            let (pp, hh) = (self.positives as i128, self.humans as i128);
            let mut out = Variants::default();
            for c in CLASSES {
                let mut best: Option<(i128, usize, (usize, String), &str)> = None;
                for ((k, v), n) in &self.shows {
                    if *k != c {
                        continue;
                    }
                    let score = n[0] as i128 * hh - n[1] as i128 * pp;
                    if score <= 0 {
                        continue;
                    }
                    let ord = Self::order_key(c, v);
                    let better = match &best {
                        None => true,
                        Some((s, p, o, _)) => score > *s || (score == *s && (n[0] > *p || (n[0] == *p && ord < *o))),
                    };
                    if better {
                        best = Some((score, n[0], ord, v.as_str()));
                    }
                }
                out.set(c, best.map(|b| b.3))?;
            }
            Ok(out)
        }

        /// The audit record: per class, every shown variant's counts,
        /// shares and score, and the chosen variant; then `variants`, the
        /// S0-13 value. No text.
        pub fn report(&self) -> Result<Value> {
            let chosen = self.variants()?;
            let (p, h) = (self.positives, self.humans);
            let mut classes = Map::new();
            for c in CLASSES {
                let mut rows: Vec<(&String, &[usize; 2])> = self.shows.iter().filter(|((k, _), _)| *k == c).map(|((_, v), n)| (v, n)).collect();
                rows.sort_by_key(|(v, _)| Self::order_key(c, v));
                let vs: Vec<Value> = rows
                    .iter()
                    .map(|(v, n)| {
                        let (ps, hs) = (n[0] as f64 / p as f64, n[1] as f64 / h as f64);
                        json!({"variant": v, "positives_showing": n[0], "humans_showing": n[1], "positive_share": ps, "human_share": hs,
                            "difference": ps - hs, "score_numerator": (n[0] as i128 * h as i128 - n[1] as i128 * p as i128).to_string()})
                    })
                    .collect();
                classes.insert(c.name().into(), json!({"group": c.group().kind(), "variants": vs, "chosen": chosen.name(c)}));
            }
            Ok(json!({"schema": AUDIT_SCHEMA, "profile": PROFILE, "statistic": STATISTIC, "positives": p, "humans": h,
                "classes": classes, "variants": chosen.to_json()}))
        }
    }

    fn file_sha(p: &Path) -> Result<String> {
        let mut f = fs::File::open(p).with_context(|| format!("open {}", p.display()))?;
        let mut h = Sha256::new();
        let mut buf = vec![0u8; 1 << 20];
        loop {
            let n = f.read(&mut buf)?;
            if n == 0 {
                break;
            }
            h.update(&buf[..n]);
        }
        Ok(hex::encode(h.finalize()))
    }

    /// Reads the rows of an assembler Train split directory `dir`, as
    /// `grammar-a-assemble`'s `supervision::for_train_rows` does for S0-8:
    /// `MANIFEST.json` must be an unsealed Train split manifest whose
    /// `rows.jsonl` digest and row count match, and a row that is not a Train
    /// row stops the read before any later line. Calls `f(line number,
    /// row)`; returns the input record (digests and counts only).
    pub fn for_train_split(dir: &Path, mut f: impl FnMut(usize, &Value) -> Result<()>) -> Result<Value> {
        let mpath = dir.join("MANIFEST.json");
        let mbytes = fs::read(&mpath).with_context(|| format!("read {}", mpath.display()))?;
        let m: Value = serde_json::from_slice(&mbytes).context("MANIFEST.json")?;
        ensure!(m["schema"] == TRAIN_MANIFEST_SCHEMA, "{} is not an assembler split manifest", mpath.display());
        ensure!(m["split"] == "train" && m["sealed"] != true, "{} is not an unsealed Train split", dir.display());
        let rows_path = dir.join("rows.jsonl");
        let rows_sha = file_sha(&rows_path)?;
        ensure!(m["files"]["rows.jsonl"] == rows_sha.as_str(), "{} differs from its manifest", rows_path.display());
        let want = m["rows"].as_u64().context("the manifest's row count")?;
        let rd = BufReader::with_capacity(1 << 20, fs::File::open(&rows_path)?);
        let mut n = 0u64;
        for (i, line) in rd.lines().enumerate() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            let v: Value = serde_json::from_str(&line).with_context(|| format!("rows.jsonl line {}", i + 1))?;
            if v["split"] != "train" {
                bail!("rows.jsonl line {} is not a Train row: stopped before reading further", i + 1);
            }
            f(i + 1, &v).with_context(|| format!("rows.jsonl line {}", i + 1))?;
            n += 1;
        }
        ensure!(n == want, "rows.jsonl holds {n} rows, its manifest says {want}");
        Ok(json!({"train_dir": dir, "manifest_sha256": sha(&mbytes), "rows_sha256": rows_sha, "rows": n,
            "build_id": m["build_id"], "row_schema": m["row_schema"]}))
    }

    /// Whether the audit takes a row of `origin` for its role: humans take
    /// human-only rows; positives take mixed and model-only rows.
    pub fn role_takes(positive: bool, origin: &str) -> bool {
        if positive { origin == "mixed" || origin == "model_only" } else { origin == "human_only" }
    }

    /// The S0-13 audit over assembler Train split directories: the humans
    /// are the positive-weight human-only rows of `humans`, the positives
    /// the positive-weight mixed and model-only rows of `positives` (see
    /// `INPUT_RULE`). Returns the audit record with every input bound by
    /// manifest and rows digest.
    pub fn audit_train_splits(humans: &[PathBuf], positives: &[PathBuf]) -> Result<Value> {
        ensure!(!humans.is_empty() && !positives.is_empty(), "the audit needs --humans and --positives Train split directories");
        let mut audit = InjectAudit::default();
        let mut inputs = Vec::new();
        for (role, dirs, positive) in [("humans", humans, false), ("positives", positives, true)] {
            let mut ids = BTreeSet::new();
            for d in dirs {
                let (mut used, mut zero) = (0usize, 0usize);
                let mut other: BTreeMap<String, usize> = BTreeMap::new();
                let input = for_train_split(d, |_, r| {
                    let (id, origin, _) = row_parts(r)?;
                    if !role_takes(positive, origin) {
                        *other.entry(origin.to_string()).or_default() += 1;
                        return Ok(());
                    }
                    let w = r["weight"].as_f64().with_context(|| format!("row {id}: weight"))?;
                    ensure!(w.is_finite() && w >= 0.0, "row {id}: weight must be finite and >= 0");
                    if w == 0.0 {
                        zero += 1;
                        return Ok(());
                    }
                    ensure!(ids.insert(id.clone()), "duplicate {role} row id {id}");
                    audit.add(positive, r["text"].as_str().context("text")?);
                    used += 1;
                    Ok(())
                })
                .with_context(|| format!("{role} {}", d.display()))?;
                inputs.push(json!({"role": role, "input": input, "rows_used": used, "rows_zero_weight": zero, "rows_other_origin": other}));
            }
        }
        let mut rep = audit.report()?;
        rep["input_rule"] = json!(INPUT_RULE);
        rep["inputs"] = json!(inputs);
        Ok(rep)
    }

    // ------------------------------------------------------------ edits

    /// Where an edit sits among edits at the same offset: an insertion that
    /// belongs to the word before it, one that belongs to the word after
    /// it, then a replacement starting there.
    #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
    enum Attach {
        Left,
        Right,
        Replace,
    }

    #[derive(Clone, Debug)]
    struct Edit {
        a: usize,
        b: usize,
        new: String,
        attach: Attach,
        class: Class,
    }

    impl Edit {
        /// A replacement that removes whitespace between words and adds
        /// none, so it joins words (a spaced dash rendered closed).
        fn joins(&self, text: &str) -> bool {
            text[self.a..self.b].contains(char::is_whitespace) && !self.new.contains(char::is_whitespace)
        }
    }

    /// The replacement of site `old` (at byte `a`) by `new`, less the
    /// whitespace both share at either end: a spaced dash rendered in
    /// another spaced style replaces its glyphs only, so the words around it
    /// keep their offsets. The kept part of each side is never empty (the
    /// styles differ in their glyphs).
    fn trimmed(a: usize, old: &str, new: &str, class: Class) -> Edit {
        let same_ws = |(x, y): &(char, char)| x == y && x.is_whitespace();
        let pre: usize = old.chars().zip(new.chars()).take_while(same_ws).map(|(x, _)| x.len_utf8()).sum();
        let (o, n) = (&old[pre..], &new[pre..]);
        let suf: usize = o.chars().rev().zip(n.chars().rev()).take_while(same_ws).map(|(x, _)| x.len_utf8()).sum();
        let (o, n) = (&o[..o.len() - suf], &n[..n.len() - suf]);
        if o.is_empty() || n.is_empty() {
            return Edit { a, b: a + old.len(), new: new.to_string(), attach: Attach::Replace, class };
        }
        Edit { a: a + pre, b: a + pre + o.len(), new: n.to_string(), attach: Attach::Replace, class }
    }

    /// V21 plan key: lowercase hex SHA-256 of
    /// `"slop_ninja_a_comp_r2_plan_v1:" + x + ":" + id`.
    pub fn plan_key(x: &str, id: &str) -> String {
        hex::encode(Sha256::digest(format!("{R2_PLAN_KEY_PREFIX}{x}:{id}").as_bytes()))
    }

    fn sha(b: &[u8]) -> String {
        hex::encode(Sha256::digest(b))
    }

    /// The conventions a C twin renders in (ranges are left as they are).
    fn conventions_of(v: &Variants) -> Conventions {
        let q = Choice { value: v.quote, source: "h_inj" };
        Conventions {
            double: q,
            single: q,
            dash: Choice { value: v.dash, source: "h_inj" },
            range: Choice { value: None, source: "none" },
            ellipsis: Choice { value: v.ellipsis, source: "h_inj" },
        }
    }

    /// One line's convention edits: its dash and ellipsis sites rendered in
    /// the variants, then its quotes, as the child normaliser renders a line
    /// (`render_sites`, then `render_quotes` on the result). A site that
    /// overlaps the previously replaced site is skipped and counted
    /// (`render_sites` cannot replace both). The edits are checked to
    /// rebuild that rendering exactly.
    fn line_edits(text: &str, ls: usize, le: usize, conv: &Conventions, out: &mut Vec<Edit>, overlaps: &mut usize) -> Result<()> {
        let seg = &text[ls..le];
        let mut reps: Vec<(usize, usize, &'static str, Class)> = Vec::new();
        for s in conventions::sites(seg) {
            let r = match s {
                Site::Dash(a, b, d) => conv.dash.value.filter(|t| *t != d).map(|t| (a, b, t.render(), Class::Dash)),
                Site::Range(..) => None,
                Site::Ellipsis(a, b, e) => conv.ellipsis.value.filter(|t| *t != e).map(|t| (a, b, if t == Ellipsis::Char { "\u{2026}" } else { "..." }, Class::Ellipsis)),
            };
            if let Some(r) = r {
                if reps.last().is_some_and(|p| r.0 < p.1) {
                    *overlaps += 1;
                    continue;
                }
                reps.push(r);
            }
        }
        let mut s1 = String::with_capacity(seg.len() + 8);
        let mut cur = 0;
        for &(a, b, r, _) in &reps {
            s1.push_str(&seg[cur..a]);
            s1.push_str(r);
            cur = b;
        }
        s1.push_str(&seg[cur..]);
        let s2: Vec<char> = conventions::render_quotes(&s1, conv, &mut Changes::default()).chars().collect();
        let mut mine: Vec<Edit> = reps.iter().map(|&(a, b, r, c)| trimmed(ls + a, &seg[a..b], r, c)).collect();
        let (mut i, mut j, mut k) = (0usize, 0usize, 0usize);
        while i < seg.len() {
            if k < reps.len() && reps[k].0 == i {
                let n = reps[k].2.chars().count();
                ensure!(s2.get(j..j + n).is_some_and(|w| w.iter().copied().eq(reps[k].2.chars())), "quote rendering changed a rendered site");
                j += n;
                i = reps[k].1;
                k += 1;
                continue;
            }
            let c = seg[i..].chars().next().expect("char");
            let r = *s2.get(j).context("quote rendering changed the line length")?;
            if r != c {
                mine.push(Edit { a: ls + i, b: ls + i + c.len_utf8(), new: r.to_string(), attach: Attach::Replace, class: Class::Quote });
            }
            i += c.len_utf8();
            j += 1;
        }
        ensure!(j == s2.len(), "quote rendering changed the line length");
        mine.sort_by_key(|e| e.a);
        let rebuilt = apply_edits(seg, &mine.iter().map(|e| Edit { a: e.a - ls, b: e.b - ls, ..e.clone() }).collect::<Vec<_>>())?;
        ensure!(rebuilt.chars().eq(s2.iter().copied()), "line edits do not rebuild the rendering");
        out.extend(mine);
        Ok(())
    }

    /// Whitespace words of `text` as byte ranges.
    fn words(text: &str) -> Vec<(usize, usize)> {
        let mut out = Vec::new();
        let mut start = None;
        for (i, c) in text.char_indices() {
            if c.is_whitespace() {
                if let Some(s) = start.take() {
                    out.push((s, i));
                }
            } else if start.is_none() {
                start = Some(i);
            }
        }
        if let Some(s) = start {
            out.push((s, text.len()));
        }
        out
    }

    /// The emphasis phrase: two consecutive words of one line, each made of
    /// letters and digits only; among them, the one whose first word has the
    /// smallest V21 key (x = `PHRASE_KEY`, d.id = `<row key>:<word index>`).
    /// Returns (word index, candidates).
    fn phrase(text: &str, ws: &[(usize, usize)], row_key: &str) -> (Option<usize>, usize) {
        let plain = |r: (usize, usize)| text[r.0..r.1].chars().all(char::is_alphanumeric);
        let mut best: Option<(String, usize)> = None;
        let mut n = 0;
        for k in 0..ws.len().saturating_sub(1) {
            if plain(ws[k]) && plain(ws[k + 1]) && !text[ws[k].1..ws[k + 1].0].contains('\n') {
                n += 1;
                let key = plan_key(PHRASE_KEY, &format!("{row_key}:{k}"));
                if best.as_ref().is_none_or(|b| (key.as_str(), k) < (b.0.as_str(), b.1)) {
                    best = Some((key, k));
                }
            }
        }
        (best.map(|b| b.1), n)
    }

    /// The keyed half of `n` content lines: the first `ceil(n / 2)` line
    /// indices in ascending order of the V21 key (x = `LINE_KEY`, d.id =
    /// `<row key>:<line index>`), returned in line order.
    pub fn keyed_half(n: usize, row_key: &str) -> Vec<usize> {
        let mut ranked: Vec<(String, usize)> = (0..n).map(|i| (plan_key(LINE_KEY, &format!("{row_key}:{i}")), i)).collect();
        ranked.sort();
        let mut out: Vec<usize> = ranked.into_iter().take(n.div_ceil(2)).map(|x| x.1).collect();
        out.sort_unstable();
        out
    }

    /// Applies edits sorted by (offset, attachment), refusing an edit that
    /// starts inside an earlier replacement or off a character boundary.
    fn apply_edits(text: &str, edits: &[Edit]) -> Result<String> {
        let mut out = String::with_capacity(text.len() + 64);
        let mut cur = 0;
        for e in edits {
            ensure!(e.a <= e.b && e.b <= text.len() && text.is_char_boundary(e.a) && text.is_char_boundary(e.b), "edit off the text");
            ensure!(e.a >= cur, "overlapping edits at byte {}", e.a);
            out.push_str(&text[cur..e.a]);
            out.push_str(&e.new);
            cur = e.b;
        }
        out.push_str(&text[cur..]);
        Ok(out)
    }

    /// The new offset of span boundary `x`: insertions at `x` that belong to
    /// the word before go before it, others after it; a replacement starting
    /// at `x` follows it; a boundary inside a (whitespace) replacement moves
    /// to the replacement's start. The text's end therefore stays the end:
    /// no edit starts after it, and an insertion there belongs to the word
    /// before.
    fn remap(x: usize, edits: &[Edit]) -> usize {
        let mut off: isize = 0;
        for e in edits {
            if e.a == e.b {
                if e.a < x || (e.a == x && e.attach == Attach::Left) {
                    off += e.new.len() as isize;
                } else if e.a > x {
                    break;
                }
            } else if e.b <= x {
                off += e.new.len() as isize - (e.b - e.a) as isize;
            } else if e.a < x {
                return (e.a as isize + off) as usize;
            } else {
                break;
            }
        }
        (x as isize + off) as usize
    }

    /// Alphanumeric runs with their labels (inside a span or not).
    fn runs(text: &str, spans: &[(usize, usize)]) -> Vec<(String, bool)> {
        let mut out = Vec::new();
        let mut start: Option<usize> = None;
        let close = |s: usize, e: usize, out: &mut Vec<(String, bool)>| out.push((text[s..e].to_string(), spans.iter().any(|&(a, b)| a <= s && e <= b)));
        for (i, c) in text.char_indices() {
            if c.is_alphanumeric() {
                start.get_or_insert(i);
            } else if let Some(s) = start.take() {
                close(s, i, &mut out);
            }
        }
        if let Some(s) = start {
            close(s, text.len(), &mut out);
        }
        out
    }

    /// One injection twin.
    #[derive(Clone, Debug)]
    pub struct Injected {
        pub text: String,
        /// Model-written byte spans of `text`, remapped exactly.
        pub spans: Vec<(usize, usize)>,
        /// Per-twin record (`slop_ninja_a_form_inject_v1`), no text.
        pub record: Value,
    }

    /// Applies group `group`'s variants to `text` (see the module notes).
    /// `spans` are the model-written byte spans (empty for a human text).
    /// Deterministic in (group, variants, row key, text); labels enter only
    /// the joining-dash guard.
    pub fn inject(text: &str, spans: &[(usize, usize)], group: Group, v: &Variants, row_key: &str) -> Result<Injected> {
        let labels = word_labels(text, spans)?;
        let before = runs(text, spans);
        let mut edits: Vec<Edit> = Vec::new();
        let mut overlaps = 0usize;
        let mut emphasis = json!(null);
        let mut layout = json!(null);
        match group {
            Group::C => {
                let conv = conventions_of(v);
                let mut ls = 0;
                loop {
                    let le = text[ls..].find('\n').map_or(text.len(), |k| ls + k);
                    line_edits(text, ls, le, &conv, &mut edits, &mut overlaps)?;
                    if le == text.len() {
                        break;
                    }
                    ls = le + 1;
                }
                if let Some(m) = v.emphasis {
                    let ws = words(text);
                    let (k, n) = phrase(text, &ws, row_key);
                    if let Some(k) = k {
                        edits.push(Edit { a: ws[k].0, b: ws[k].0, new: m.into(), attach: Attach::Right, class: Class::Emphasis });
                        edits.push(Edit { a: ws[k + 1].1, b: ws[k + 1].1, new: m.into(), attach: Attach::Left, class: Class::Emphasis });
                    }
                    emphasis = json!({"placed": k.is_some(), "candidates": n, "word_index": k});
                }
            }
            Group::L => {
                let m = Measure::of(text);
                let (mut selected, mut had, mut already) = (0usize, 0usize, 0usize);
                if let Some(t) = &v.trailing_whitespace {
                    // Each selected line's trailing whitespace, less its
                    // final '\r' run (the line ending), becomes the variant.
                    for i in keyed_half(m.lines.len(), row_key) {
                        selected += 1;
                        let cur = trailing_variant(m.trail(i));
                        had += usize::from(!cur.is_empty());
                        if cur == t {
                            already += 1;
                            continue;
                        }
                        let a = m.lines[i].body.1;
                        let attach = if cur.is_empty() { Attach::Left } else { Attach::Replace };
                        edits.push(Edit { a, b: a + cur.len(), new: t.clone(), attach, class: Class::TrailingWhitespace });
                    }
                }
                let (mut boundaries, mut replaced, mut crlf) = (0usize, 0usize, 0usize);
                if let Some(s) = &v.separator {
                    // Every paragraph boundary (blank-line run, sentence
                    // break, line break) takes the blank-line variant; wraps
                    // stay.
                    for (i, g) in m.gaps.iter().enumerate() {
                        if g.boundary() {
                            boundaries += 1;
                            let after_crlf = m.trail(i).ends_with('\r');
                            crlf += usize::from(after_crlf);
                            let target = separator_after(s, after_crlf);
                            if m.separator(i) != target {
                                replaced += 1;
                                edits.push(Edit { a: g.range.0, b: g.range.1, new: target, attach: Attach::Replace, class: Class::Separator });
                            }
                        }
                    }
                }
                layout = json!({"content_lines": m.lines.len(), "line_form": m.line_form(), "hard_wrapped": m.hard_wrapped,
                    "lines_selected": selected, "selected_lines_with_trailing_whitespace": had, "selected_lines_already_in_variant": already,
                    "boundaries": boundaries, "boundaries_after_crlf_lines": crlf, "separators_replaced": replaced});
            }
        }
        // The one label-aware rule: a joining replacement never takes a span edge.
        let edges: BTreeSet<usize> = spans.iter().flat_map(|&(a, b)| [a, b]).collect();
        let mut guard: BTreeMap<&'static str, usize> = BTreeMap::new();
        edits.retain(|e| {
            let hit = e.joins(text) && edges.range(e.a..=e.b).next().is_some();
            if hit {
                *guard.entry(e.class.name()).or_default() += 1;
            }
            !hit
        });
        edits.sort_by_key(|e| (e.a, e.attach));
        let out = apply_edits(text, &edits)?;
        let applied = edits;
        let new_spans: Vec<(usize, usize)> = spans.iter().map(|&(a, b)| (remap(a, &applied), remap(b, &applied))).collect();
        let new_labels = word_labels(&out, &new_spans).context("remapped spans")?;
        ensure!(runs(&out, &new_spans) == before, "the word sequence or its labels changed");
        if group == Group::L {
            ensure!(out.split_whitespace().eq(text.split_whitespace()), "a layout twin changed a word");
        }
        let m0 = labels.iter().filter(|w| w.1 == 1).count();
        let m1 = new_labels.iter().filter(|w| w.1 == 1).count();
        ensure!((m0 == 0) == (m1 == 0) && (m0 == labels.len()) == (m1 == new_labels.len()), "the injection changed the row's origin");
        let mut by: BTreeMap<&'static str, usize> = BTreeMap::new();
        for e in &applied {
            *by.entry(e.class.name()).or_default() += 1;
        }
        let record = json!({
            "schema": RECORD_SCHEMA,
            "profile": PROFILE,
            "group": group.kind(),
            "variants": v.group_json(group),
            "row_key": row_key,
            "source_text_sha256": sha(text.as_bytes()),
            "text_sha256": sha(out.as_bytes()),
            "text_changed": out != text,
            "words_before": labels.len(),
            "words_after": new_labels.len(),
            "model_words_before": m0,
            "model_words_after": m1,
            "alphanumeric_runs": before.len(),
            "edits_by_class": by,
            "span_guard_skips": guard,
            "overlapping_sites_skipped": overlaps,
            "emphasis": emphasis,
            "layout": layout,
            "plan_keys": {"lines": format!("V21 x = {LINE_KEY}, d.id = <row key>:<content line index>; first ceil(n/2) in ascending key order"),
                "phrase": format!("V21 x = {PHRASE_KEY}, d.id = <row key>:<word index>; smallest key")},
            "label_blind_draws": true,
        });
        Ok(Injected { text: out, spans: new_spans, record })
    }

    // ------------------------------------------------------------ twins

    /// The two H-inj twins of a human-only row, as (manifest line, twin row)
    /// pairs, C first; none for other origins. `values_sha256` is the digest
    /// of the value register the variants were read from. Each twin row is the row with
    /// the twin's text and digest, no spans, share 0 (and `word_count` when
    /// present) under the row's own field names, a new id
    /// (`probe:<kind>:<id>`), `probe_of` and the `form_inject` record.
    pub fn twins_for_row(row: &Value, v: &Variants, values_sha256: &str) -> Result<Vec<(Value, Value)>> {
        let (id, origin, spans) = row_parts(row)?;
        if origin != "human_only" {
            return Ok(Vec::new());
        }
        ensure!(spans.is_empty(), "row {id}: a human-only row with spans");
        let text = row["text"].as_str().expect("checked text");
        let base_sha = sha(text.as_bytes());
        let mut out = Vec::new();
        for g in GROUPS {
            let inj = inject(text, &[], g, v, &id).with_context(|| format!("row {id}"))?;
            let pid = format!("probe:{}:{id}", g.kind());
            let words = inj.record["words_after"].as_u64().unwrap_or(0) as usize;
            let a = Augmented { text: inj.text, spans: inj.spans, words, model_words: 0, record: inj.record };
            let mut pr = rebuild(row, pid.clone(), origin, &a, "form_inject")?;
            pr["probe_of"] = json!(id);
            let r = &a.record;
            let m = json!({"schema": PROBE_SCHEMA, "probe_id": pid, "base_id": id, "split": row["split"], "kind": g.kind(), "origin": origin,
                "base_text_sha256": base_sha, "text_sha256": r["text_sha256"], "text_changed": r["text_changed"], "profile": PROFILE,
                "variants": r["variants"], "values_sha256": values_sha256, "edits_by_class": r["edits_by_class"],
                "span_guard_skips": r["span_guard_skips"], "overlapping_sites_skipped": r["overlapping_sites_skipped"],
                "words_before": r["words_before"], "words_after": r["words_after"]});
            out.push((m, pr));
        }
        Ok(out)
    }

    /// Whether the variants inject anything for group `g`.
    pub fn injects(v: &Variants, g: Group) -> bool {
        v.any(g)
    }

    /// The frozen variants: the S0-13 entry of the round-2 value register
    /// (`VALUES-R2.json`, protocol A2 §2.11, §12.6). Refuses a document that
    /// is not the register or holds no S0-13 entry with a `variants` object
    /// naming the six classes. Returns the variants and the entry.
    pub fn variants_from_values(doc: &Value) -> Result<(Variants, Value)> {
        ensure!(doc["schema"] == VALUES_SCHEMA, "not the round-2 value register (schema {VALUES_SCHEMA})");
        let e = doc["values"].get(VALUES_KEY).with_context(|| format!("the value register holds no {VALUES_KEY} entry: no H-inj twin before the variants are frozen"))?;
        ensure!(e.is_object(), "{VALUES_KEY} is not an object");
        let v = Variants::from_json(e.get("variants").with_context(|| format!("{VALUES_KEY} has no `variants` member"))?)?;
        Ok((v, e.clone()))
    }

    /// The counts of one twin run (`twin_rows`).
    #[derive(Debug, Default)]
    pub struct TwinCounts {
        pub rows: usize,
        pub twins_by_kind: BTreeMap<String, usize>,
        pub twins_with_unchanged_text: BTreeMap<String, usize>,
        pub edits_by_kind_and_class: BTreeMap<String, BTreeMap<String, u64>>,
        pub rows_without_twins_by_origin: BTreeMap<String, usize>,
    }

    /// The twins of every human-only row of split `split` in `rows`
    /// (JSONL), each passed to `sink(manifest line, twin row)`. Every row
    /// must be of `split`: a row of another split stops the run before any
    /// later line. Row ids must be unique.
    pub fn twin_rows(rows: impl BufRead, split: &str, v: &Variants, values_sha256: &str, mut sink: impl FnMut(&Value, &Value) -> Result<()>) -> Result<TwinCounts> {
        let mut c = TwinCounts::default();
        let mut ids = BTreeSet::new();
        for (i, l) in rows.lines().enumerate() {
            let l = l?;
            if l.trim().is_empty() {
                continue;
            }
            let r: Value = serde_json::from_str(&l).with_context(|| format!("line {}", i + 1))?;
            ensure!(r["split"] == split, "line {} is a row of split {} among the rows of split {split}: stopped before reading further", i + 1, r["split"]);
            let id = r["id"].as_str().with_context(|| format!("line {}: id", i + 1))?.to_owned();
            ensure!(ids.insert(id.clone()), "duplicate row id {id}");
            c.rows += 1;
            let twins = twins_for_row(&r, v, values_sha256)?;
            if twins.is_empty() {
                *c.rows_without_twins_by_origin.entry(r["origin"].as_str().unwrap_or("?").into()).or_default() += 1;
            }
            for (m, p) in twins {
                let kind = m["kind"].as_str().context("kind")?.to_string();
                *c.twins_by_kind.entry(kind.clone()).or_default() += 1;
                *c.twins_with_unchanged_text.entry(kind.clone()).or_default() += usize::from(m["text_changed"] == false);
                for (cl, n) in m["edits_by_class"].as_object().into_iter().flatten() {
                    *c.edits_by_kind_and_class.entry(kind.clone()).or_default().entry(cl.clone()).or_default() += n.as_u64().unwrap_or(0);
                }
                sink(&m, &p)?;
            }
        }
        Ok(c)
    }
}

#[cfg(test)]
mod tests;
