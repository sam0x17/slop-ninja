//! Cohort generation profiles and their ids, and the every-class step at
//! assembly.
//!
//! * `raw` (era-v1): no post-processing, no every-class step.
//! * `cc_era_parent_norm_v1` (format-v1): the normaliser and form screen;
//!   the every-class step is recorded but excludes nothing.
//! * `cc_cohort_profile_v1` (the default): the normaliser and form screen,
//!   plus the final-document length window `cc_final_length_window_v1` in the
//!   acceptance path; at assembly every document of every class goes through
//!   the excerpt layout, the paragraph screen and the length window, and a
//!   document that fails any of them leaves the cohort whatever its class.
//! * `cc_cohort_profile_v2` (protocol A2 §6.2, as amended by amendments 7,
//!   10, 13, 29, 30 and 38): v1 with the parent-conditioned layout transfer
//!   (`transfer::LAYOUT_TRANSFER_ID`, measured by `layout_measure_v2`) in place of v1's
//!   layout step, the licence screen `cc_form_licence_screen_v1` on every
//!   prose-path child (line-form children stay unscreened), the echo screen
//!   `cc_echo_screen_v1` on every child with a parent, and keyed convention
//!   draws from the prior of the child's own population and domain
//!   (`PriorV2`, `resolve_v2`) under the population's own seed
//!   (`norm_prior_seed`; population ids from the closed list
//!   `R2_POPULATIONS`). Entry points: `child_v2` (a
//!   mirror, or one replaced span), `item_v2` (a spliced item: any number of
//!   edits, insertions included, laid out in parent order and screened on the
//!   assembled item; amendment 29 item 29.1), `edit_in_item_v2` (one edit of
//!   such an item judged before its siblings, laid out in its item) and
//!   `donor_child_v2` (a parentless row laid out like a layout donor). The
//!   every-class step at assembly is v1's in the C block (`every_class`); in
//!   the T block it is the length window alone (`every_class_t_block`), so
//!   humans are never laid out.
//!
//! A human-only document is its own parent, so the normaliser's target form
//! is the document itself and its text is never rewritten.
//!
//! Moved from `grammar-cc-human`'s `era_norm` (v1), which re-exports it; the
//! errors are typed here and keep v1's messages.
use crate::conventions::{Conventions, Counts, Prior, resolve};
use crate::echo::{self, ECHO_SCREEN_ID};
use crate::layout::{EXCERPT_LAYOUT_ID, excerpt_layout};
use crate::measure::{MEASURE_ID, Measure, escape_ws};
use crate::screen::{FORM_LICENCE_SCREEN_ID, classify_all, paragraph_screen, v1_reason};
use crate::text;
use crate::transfer::{LAYOUT_TRANSFER_ID, LineContext, Path as TransferPath, SourceLayout, Transferred, licensed_line_reasons, transfer};
use crate::window::{self, LENGTH_WINDOW, LENGTH_WINDOW_ID};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fmt;
use std::path::Path;

pub const NORM_ID: &str = "cc_era_parent_norm_v1";
pub const PROFILE_V1: &str = "cc_cohort_profile_v1";
pub const PROFILE_V2: &str = "cc_cohort_profile_v2";
pub const PROFILE_RAW: &str = "raw";
/// How the licence screen reads a child's final paragraphs (amendment 13
/// item 13.1): by construction, never by measuring the document again.
pub const SCREEN_READING: &str = "construction";

/// Cohort generation profile (see the module notes).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Profile {
    /// era-v1: no post-processing, no every-class step.
    Raw,
    /// format-v1: normaliser and form screen; every-class step recorded only.
    NormV1,
    /// The default: normaliser, form screen and length window; every-class
    /// failures leave the cohort.
    CohortV1,
    /// Protocol A2 §6.2 (amendments 7 and 10): v1 with the layout
    /// transfer, the licence screen on the prose path, the echo screen and
    /// per-population x domain priors.
    CohortV2,
}

impl Profile {
    pub const DEFAULT: Profile = Profile::CohortV1;

    pub fn parse(s: &str) -> Result<Profile, ProfileError> {
        Ok(match s {
            PROFILE_RAW => Profile::Raw,
            NORM_ID => Profile::NormV1,
            PROFILE_V1 => Profile::CohortV1,
            PROFILE_V2 => Profile::CohortV2,
            _ => return Err(ProfileError::Unknown(s.to_string())),
        })
    }
    pub fn id(self) -> &'static str {
        match self {
            Profile::Raw => PROFILE_RAW,
            Profile::NormV1 => NORM_ID,
            Profile::CohortV1 => PROFILE_V1,
            Profile::CohortV2 => PROFILE_V2,
        }
    }
    pub fn normalises(self) -> bool {
        self != Profile::Raw
    }
    /// The final-document word window of the acceptance path and assembly.
    pub fn window(self) -> Option<(usize, usize)> {
        matches!(self, Profile::CohortV1 | Profile::CohortV2).then_some(LENGTH_WINDOW)
    }
    /// Whether a document failing the every-class step leaves the cohort.
    pub fn excludes(self) -> bool {
        matches!(self, Profile::CohortV1 | Profile::CohortV2)
    }
    /// Whether the profile lays children out with the layout transfer
    /// (`child_v2`) and draws conventions from a `PriorV2`.
    pub fn transfers(self) -> bool {
        self == Profile::CohortV2
    }
    /// A normalising profile needs the normaliser prior; `raw` takes none.
    pub fn check_prior(self, norm: Option<&Path>) -> Result<(), ProfileError> {
        match (self.normalises(), norm) {
            (true, None) => Err(ProfileError::NeedsPrior(self)),
            (false, Some(_)) => Err(ProfileError::TakesNoPrior(self)),
            _ => Ok(()),
        }
    }
    pub fn record(self) -> Value {
        if self == Profile::CohortV2 {
            let w = LENGTH_WINDOW;
            return json!({"profile": self.id(), "normaliser": NORM_ID, "layout_measure": MEASURE_ID, "layout_transfer": LAYOUT_TRANSFER_ID, "echo_screen": ECHO_SCREEN_ID,
                "length_window": {"id": LENGTH_WINDOW_ID, "min_words": w.0, "max_words": w.1, "unit": "whitespace words of the final document"},
                "form_screen": {"id": FORM_LICENCE_SCREEN_ID, "reading": SCREEN_READING, "rule": "prose-path children: a reason outside the source body's licence (classify_all over its body paragraphs) in a final body paragraph holding generated text (read by construction: the parent's own boundaries for parent text, the transfer's units for generated text), a raw child body line's list or sentence-end reason outside it, or a reason outside the frame licence in a child frame line rejects; a source passing the screen has no frame and an empty licence, so its children get section 6.2's screen; line-form children are not screened (would-be reasons counted)"},
                "prior": "keyed draws (the population's own seed, norm_prior_seed) from the child's population x domain prior; the population-wide prior where the domain cell holds no parent with the convention; then none",
                "every_class": "C block: excerpt layout, paragraph screen and length window on every document; T block: the length window only; a failure leaves the cohort whatever the class"});
        }
        json!({"profile": self.id(), "normaliser": self.normalises().then_some(NORM_ID),
            "length_window": self.window().map(|w| json!({"id": LENGTH_WINDOW_ID, "min_words": w.0, "max_words": w.1, "unit": "whitespace words of the final document"})),
            "every_class": match self {
                Profile::Raw => "none",
                Profile::NormV1 => "excerpt layout and paragraph screen on every document, recorded",
                Profile::CohortV1 | Profile::CohortV2 => "excerpt layout, paragraph screen and length window on every document; a failure leaves the cohort whatever the class",
            }})
    }
}

/// A profile name or prior argument the profile does not accept.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProfileError {
    Unknown(String),
    NeedsPrior(Profile),
    TakesNoPrior(Profile),
}

impl fmt::Display for ProfileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            // The message is pinned by the v1 golden digests (`profile/unknown`).
            ProfileError::Unknown(s) => write!(f, "unknown cohort profile {s} (known: {PROFILE_V1}, {NORM_ID}, {PROFILE_RAW})"),
            ProfileError::NeedsPrior(p) => write!(f, "profile {} needs the normaliser prior (--norm)", p.id()),
            ProfileError::TakesNoPrior(p) => write!(f, "profile {} takes no normaliser prior", p.id()),
        }
    }
}

impl std::error::Error for ProfileError {}

/// Every-class step at assembly: the excerpt layout, the paragraph screen
/// and (when the profile has one) the length window. Returns (text, record,
/// failure reasons); a profile that excludes drops a document with any reason.
pub fn every_class(text: &str, profile: Profile) -> (String, Value, Vec<String>) {
    let t = excerpt_layout(text);
    let changed = t != text;
    let screen = paragraph_screen(&t, None);
    let words = text::words(&t);
    let mut reasons = screen.clone();
    let mut rec = json!({"profile": profile.id(), "excerpt_layout": EXCERPT_LAYOUT_ID, "layout_changed": changed, "paragraph_screen": screen, "words": words});
    if let Some(w) = profile.window() {
        let r = window::reasons(words, w);
        rec["length_window"] = json!({"id": LENGTH_WINDOW_ID, "min_words": w.0, "max_words": w.1, "pass": r.is_empty()});
        reasons.extend(r);
    }
    rec["excluded"] = json!(profile.excludes() && !reasons.is_empty());
    (t, rec, reasons)
}

/// The T block's every-class step under profile v2: the length window only.
/// Humans are never laid out, so the text is unchanged. Returns (record,
/// failure reasons); a document with any reason leaves the population.
pub fn every_class_t_block(text: &str) -> (Value, Vec<String>) {
    let words = text::words(text);
    let w = LENGTH_WINDOW;
    let reasons = window::reasons(words, w);
    let rec = json!({"profile": PROFILE_V2, "block": "T", "words": words,
        "length_window": {"id": LENGTH_WINDOW_ID, "min_words": w.0, "max_words": w.1, "pass": reasons.is_empty()}, "excluded": !reasons.is_empty()});
    (rec, reasons)
}

// ------------------------------------------------------------------ v2 prior

/// Profile v2's convention prior for one population: the population-wide
/// tallies and one cell per domain, each as `Prior::of` over that
/// population's own parents. No other population's parent informs it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PriorV2 {
    pub population: String,
    pub prior: Prior,
    pub by_domain: BTreeMap<String, Prior>,
}

impl PriorV2 {
    /// The prior of `population` from its parents as (domain, text) pairs.
    pub fn of<'a>(population: &str, parents: impl Iterator<Item = (&'a str, &'a str)>) -> PriorV2 {
        let mut pop: Vec<&str> = Vec::new();
        let mut cells: BTreeMap<String, Vec<&str>> = BTreeMap::new();
        for (d, t) in parents {
            pop.push(t);
            cells.entry(d.to_string()).or_default().push(t);
        }
        PriorV2 {
            population: population.to_string(),
            prior: Prior::of(pop.into_iter()),
            by_domain: cells.into_iter().map(|(d, ts)| (d, Prior::of(ts.into_iter()))).collect(),
        }
    }
}

fn shows<K>(m: &BTreeMap<K, usize>) -> bool {
    m.values().any(|&v| v > 0)
}

/// Conventions under profile v2: exactly `cc_era_parent_norm_v1`'s parent
/// majorities, other-quote fallback and keyed draw (`resolve`, `KEY_SCOPE`,
/// the population's seed), with each convention's draw taken from the
/// child's domain cell when that cell holds a parent with the convention,
/// else from the population-wide prior, else none (v1's default when the
/// batch prior is empty). Keyed sources read `keyed_prior_domain` or
/// `keyed_prior_population`.
pub fn resolve_v2(c: &Counts, prior: &PriorV2, domain: &str, seed: &str, doc: &str) -> Conventions {
    let empty = Prior::default();
    let cell = prior.by_domain.get(domain).unwrap_or(&empty);
    let pop = &prior.prior;
    let (q, d, r, e) = (shows(&cell.quotes), shows(&cell.dash), shows(&cell.range), shows(&cell.ellipsis));
    let eff = Prior {
        parents: cell.parents,
        quotes: if q { cell.quotes.clone() } else { pop.quotes.clone() },
        dash: if d { cell.dash.clone() } else { pop.dash.clone() },
        range: if r { cell.range.clone() } else { pop.range.clone() },
        ellipsis: if e { cell.ellipsis.clone() } else { pop.ellipsis.clone() },
    };
    let mut conv = resolve(c, &eff, seed, doc);
    let label = |from_cell: bool| if from_cell { "keyed_prior_domain" } else { "keyed_prior_population" };
    if conv.double.source == "keyed_prior" {
        conv.double.source = label(q);
    }
    if conv.single.source == "keyed_prior" {
        conv.single.source = label(q);
    }
    if conv.dash.source == "keyed_prior" {
        conv.dash.source = label(d);
    }
    if conv.range.source == "keyed_prior" {
        conv.range.source = label(r);
    }
    if conv.ellipsis.source == "keyed_prior" {
        conv.ellipsis.source = label(e);
    }
    conv
}

// ------------------------------------------------------------------ v2 seed

/// Round-2 plan keys (protocol A2 value V21, unkeyed):
/// `SHA-256("slop_ninja_a_comp_r2_plan_v1:" + x + ":" + d.id)`. A copy of
/// `grammar_a_positives::span_ops::R2_PLAN_KEY_PREFIX`, pinned to it by a
/// parity test in `grammar-cc-human`.
pub const R2_PLAN_KEY_PREFIX: &str = "slop_ninja_a_comp_r2_plan_v1:";
/// The V21 key name `x` of a population's norm-prior seed.
pub const NORM_PRIOR_KEY: &str = "norm_prior";

/// The key seed of population `population`'s convention prior (amendment 7
/// item 7.8): V21's plan key with x = `"norm_prior"` and d.id = the
/// population id, as lowercase hex. Each population has its own seed by
/// construction, and the seed is fixed before any draw uses it.
pub fn norm_prior_seed(population: &str) -> String {
    sha256_hex(format!("{R2_PLAN_KEY_PREFIX}{NORM_PRIOR_KEY}:{population}").as_bytes())
}

/// The round-2 population ids (protocol A2 amendment 10 item 10.8): one per
/// §5.1 population that gets generated positives, a closed list. A batch
/// that re-verdicts existing rows names the population they join (`train2`
/// for R0's Train positives, `cal2_t` for old CAL's). The ids may be renamed
/// by forward amendment only before the first `BATCH.json` that names a
/// population.
pub const R2_POPULATIONS: [&str; 14] = [
    "train2",
    "deviter2_t",
    "deviter2_c",
    "deviter2_mock",
    "devselect2_t",
    "devselect2_c",
    "cal2_t",
    "cal2_c",
    "cal2_mock",
    "test2_t",
    "test2_c",
    "test2_mock",
    "span_select2",
    "span_test2",
];

/// Whether `population` is on the closed list `R2_POPULATIONS`.
pub fn is_r2_population(population: &str) -> bool {
    R2_POPULATIONS.contains(&population)
}

// ------------------------------------------------------------------ v2 child

/// One child under profile v2.
#[derive(Clone, Debug)]
pub struct ChildV2 {
    /// The laid-out child (for a span, the trimmed replacement).
    pub text: String,
    /// Every rejection reason: form (the licence screen, on the prose path)
    /// and echo.
    pub reasons: Vec<String>,
    /// The profile, normaliser and transfer ids, the conventions, the
    /// source's layout class and measured properties, what the transfer
    /// applied, the screens' outcomes and the raw-response and output
    /// digests (`input_sha256` repeats the raw digest under v1's name). No
    /// text. Callers add the parent or donor id.
    pub record: Value,
}

fn sha256_hex(b: &[u8]) -> String {
    format!("{:x}", Sha256::digest(b))
}

/// The leading whitespace a replacement may hold (amendment 29 item 29.1):
/// none, or one line break followed by a lead (no other line break), when
/// a later edit of an item has no room on the line it shares with an
/// earlier edit. Returns its byte length, or None when the replacement's
/// leading whitespace is of any other form.
pub fn replacement_lead(r: &str) -> Option<usize> {
    let lead = r.len() - r.trim_start_matches(py_space).len();
    (lead == 0 || (r.starts_with('\n') && !r[1..lead].contains('\n'))).then_some(lead)
}

/// `parent[..s] + rep + parent[e..]` under `generation::assemble`'s splice
/// rules (a non-empty span on character and token boundaries, a non-empty
/// trimmed replacement, except that it may begin with one line break and a
/// lead: `replacement_lead`, amendment 29 item 29.1, which amendment 38 item
/// 38.1 also gives a single edit), with its error messages.
pub fn splice(parent: &str, (s, e): (usize, usize), rep: &str) -> Result<String, &'static str> {
    if s >= e || e > parent.len() {
        return Err("span order");
    }
    if !parent.is_char_boundary(s) || !parent.is_char_boundary(e) {
        return Err("char boundary");
    }
    if !(s == 0 || parent[..s].ends_with(char::is_whitespace)) {
        return Err("left token boundary");
    }
    if !(e == parent.len() || parent[e..].starts_with(char::is_whitespace)) {
        return Err("right token boundary");
    }
    if rep.trim().is_empty() || rep.trim_end() != rep || replacement_lead(rep).is_none() {
        return Err("replacement layout");
    }
    Ok(format!("{}{rep}{}", &parent[..s], &parent[e..]))
}

/// Python `str.isspace`: Unicode White_Space plus U+001C..U+001F, as the
/// spliced generator's assembly reads whitespace.
fn py_space(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}

/// Byte ranges of maximal runs of non-`py_space` characters.
fn py_word_ranges(s: &str) -> Vec<(usize, usize)> {
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

/// One edit's splice: the parent byte range it replaces (`s == e` for an
/// insertion) and, for an insertion, the separator assembly puts before the
/// new text (never part of a span).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Part<'a> {
    pub span: (usize, usize),
    pub insert_sep: Option<&'a str>,
}

/// An assembled item: the document and each edit's final byte span, in edit
/// order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Assembled {
    pub text: String,
    pub edit_spans: Vec<(usize, usize)>,
}

/// Splices replacements into the parent under the spliced generator's
/// assembly rules (`grammar_a_positives::splice::assemble_parts`, with its
/// error messages; a parity test in `grammar-cc-human` pins the two): edits
/// disjoint in parent order; an insertion adds its separator before the new
/// text; a replacement sits on whitespace boundaries; every replacement is
/// non-empty and trimmed, except that it may begin with one line break and
/// a lead (`replacement_lead`; amendment 29 item 29.1), which belong to no
/// edit's final span; the human characters, the final boundaries and the
/// word accounting are verified.
pub fn assemble_item(parent: &str, parts: &[Part], reps: &[&str]) -> Result<Assembled, &'static str> {
    if parts.len() != reps.len() || parts.is_empty() {
        return Err("edit/replacement count");
    }
    let mut order: Vec<usize> = (0..parts.len()).collect();
    order.sort_by_key(|&i| parts[i].span);
    let mut out = String::new();
    let mut human = String::new();
    let mut cur = 0usize;
    let mut spans = vec![(0usize, 0usize); parts.len()];
    for &i in &order {
        let (s, e) = parts[i].span;
        let r = reps[i];
        if !(s >= cur && s <= e && e <= parent.len()) {
            return Err("edit order or overlap");
        }
        if !parent.is_char_boundary(s) || !parent.is_char_boundary(e) {
            return Err("char boundary");
        }
        let Some(lead) = replacement_lead(r).filter(|_| !r.trim_matches(py_space).is_empty() && r.trim_end_matches(py_space) == r) else {
            return Err("replacement layout");
        };
        let seg = &parent[cur..s];
        out.push_str(seg);
        human.push_str(seg);
        if s == e {
            out.push_str(parts[i].insert_sep.ok_or("insert separator")?);
        } else {
            if !(s == 0 || parent[..s].ends_with(py_space)) {
                return Err("left boundary");
            }
            if !(e == parent.len() || parent[e..].starts_with(py_space)) {
                return Err("right boundary");
            }
        }
        let fs = out.len() + lead;
        out.push_str(r);
        spans[i] = (fs, out.len());
        cur = e;
    }
    out.push_str(&parent[cur..]);
    human.push_str(&parent[cur..]);
    let mut expect = String::new();
    let mut c = 0;
    for &i in &order {
        let (s, e) = parts[i].span;
        expect.push_str(&parent[c..s]);
        c = e;
    }
    expect.push_str(&parent[c..]);
    if human != expect {
        return Err("human characters changed");
    }
    let mut sorted = spans.clone();
    sorted.sort();
    if sorted.windows(2).any(|w| w[0].1 >= w[1].0) {
        return Err("final spans touch or overlap");
    }
    for &(a, b) in &sorted {
        if !(a == 0 || out[..a].ends_with(py_space)) {
            return Err("final left boundary");
        }
        if !(b == out.len() || out[b..].starts_with(py_space)) {
            return Err("final right boundary");
        }
        if out[a..b].starts_with(py_space) || out[a..b].ends_with(py_space) {
            return Err("span edge whitespace");
        }
    }
    let words = py_word_ranges(&out);
    let mut inside = 0usize;
    for &(a, b) in &words {
        if sorted.iter().any(|&(s, e)| (a < s && b > s) || (a < e && b > e)) {
            return Err("word straddles a span edge");
        }
        inside += sorted.iter().filter(|&&(s, e)| a >= s && b <= e).count();
    }
    let ai_words: usize = reps.iter().map(|r| py_word_ranges(r).len()).sum();
    if inside != ai_words {
        return Err("word accounting");
    }
    if words.is_empty() || inside >= words.len() {
        return Err("document has no human word left");
    }
    Ok(Assembled { text: out, edit_spans: spans })
}

/// One edit's place in an assembled document, for reading its paragraphs
/// by construction (amendment 13 item 13.1). Public, with
/// `constructed_paragraphs` and `overlapping`, so that the dry run's
/// counting tool reads count 6's paragraph through this code and copies no
/// rule (protocol A2 amendment 31 item 31.1); exposing them changes no
/// output.
#[derive(Clone, Copy, Debug)]
pub struct Placed<'a> {
    /// The parent byte range it replaces (`s == e` for an insertion).
    pub span: (usize, usize),
    /// The separator assembly put before an insertion's text.
    pub sep: Option<&'a str>,
    /// The replacement's byte range in the document (a leading line break
    /// and lead included; amendment 29 item 29.1).
    pub fin: (usize, usize),
    /// The replacement's units (frame line, body paragraph or kept line; each
    /// line of a line-form child) as byte ranges in the replacement.
    pub units: &'a [(usize, usize)],
}

fn trim_range(doc: &str, (a, b): (usize, usize)) -> Option<(usize, usize)> {
    let t = &doc[a..b];
    let lead = t.len() - t.trim_start().len();
    let s = t.trim();
    (!s.is_empty()).then(|| (a + lead, a + lead + s.len()))
}

/// The paragraphs of an assembled document by construction (amendment 13
/// item 13.1): the parent's own boundaries (`measure`'s reading of the
/// parent) for parent text outside the replaced spans, each edit's unit
/// boundaries for its generated text, and an insertion's separator when it
/// holds a newline. The first and last units of an edit join the parent text
/// before and after it up to the parent's nearest boundaries. The document
/// is never measured again. Returns trimmed byte ranges of `doc`, in order.
pub fn constructed_paragraphs(parent: &str, doc: &str, placed: &[Placed]) -> Vec<(usize, usize)> {
    let pm = Measure::of(parent);
    // The document offset of parent offset x: edits ending at or before x
    // (strict: before x) come before it.
    let at = |x: usize, strict: bool| -> usize {
        let mut y = x as isize;
        for p in placed {
            if p.span.1 < x || (!strict && p.span.1 == x) {
                y += (p.sep.map_or(0, str::len) + (p.fin.1 - p.fin.0)) as isize - (p.span.1 - p.span.0) as isize;
            }
        }
        y as usize
    };
    let mut cuts: Vec<(usize, usize)> = Vec::new();
    for g in pm.gaps.iter().filter(|g| g.boundary()) {
        let (g0, g1) = g.range;
        if placed.iter().any(|p| p.span.0 < p.span.1 && p.span.0 <= g0 && g1 <= p.span.1) {
            continue;
        }
        // An insertion strictly inside the gap splits it around its text.
        let mut ins: Vec<&Placed> = placed.iter().filter(|p| p.span.0 == p.span.1 && g0 < p.span.0 && p.span.0 < g1).collect();
        ins.sort_by_key(|p| p.fin.0);
        let mut c0 = at(g0, false);
        for p in ins {
            cuts.push((c0, p.fin.0 - p.sep.map_or(0, str::len)));
            c0 = p.fin.1;
        }
        cuts.push((c0, at(g1, true)));
    }
    for p in placed {
        for w in p.units.windows(2) {
            cuts.push((p.fin.0 + w[0].1, p.fin.0 + w[1].0));
        }
        if let Some(sep) = p.sep.filter(|s| s.contains('\n')) {
            cuts.push((p.fin.0 - sep.len(), p.fin.0));
        }
    }
    cuts.sort();
    let mut out = Vec::new();
    let mut cur = 0usize;
    for (c0, c1) in cuts {
        if c0 > cur {
            out.extend(trim_range(doc, (cur, c0)));
        }
        cur = cur.max(c1);
    }
    if cur < doc.len() {
        out.extend(trim_range(doc, (cur, doc.len())));
    }
    out
}

/// The paragraphs of `paras` that overlap the byte range `fin` (an
/// insertion's empty range overlaps the paragraph holding it).
pub fn overlapping(paras: &[(usize, usize)], (a, b): (usize, usize)) -> Vec<(usize, usize)> {
    paras.iter().copied().filter(|&(s, e)| s < b.max(a + 1) && a < e).collect()
}

/// The licence screen on paragraphs of an assembled document (amendment 10
/// item 10.5, read by construction under amendment 13 item 13.1): each of
/// `paras` with a reason (`classify_all`) outside `licence` gives v1's
/// first-match code of its first unlicensed reason, in `classify`'s order
/// (`form_paragraph_<reason>`, the list halves reported as
/// `list_like_line`). With an empty licence these are v1's codes exactly.
/// Returns (codes, unlicensed reasons as `body:<reason>`).
fn licence_reasons(doc: &str, paras: &[(usize, usize)], licence: &[String]) -> (Vec<String>, Vec<String>) {
    let (mut codes, mut unl) = (Vec::new(), Vec::new());
    for &(a, b) in paras {
        let bad: Vec<&str> = classify_all(&doc[a..b]).into_iter().filter(|r| !licence.iter().any(|l| l == r)).collect();
        if let Some(first) = bad.first() {
            codes.push(format!("form_paragraph_{}", v1_reason(first)));
            unl.extend(bad.iter().map(|r| format!("body:{r}")));
        }
    }
    (codes, unl)
}

/// What a re-measurement of the document would read (amendment 13 item
/// 13.1's count; it decides nothing): whether `measure`'s paragraphs over
/// `fin` differ from the constructed ones, their counts, and the codes the
/// licence screen would give on the re-measured paragraphs.
fn remeasure(doc: &str, constructed: &[(usize, usize)], fin: (usize, usize), licence: &[String]) -> Value {
    let m = Measure::of(doc);
    let re: Vec<(usize, usize)> = m.paragraphs_overlapping(fin.0, fin.1).map(|k| m.paragraphs[k].range).collect();
    let (codes, _) = licence_reasons(doc, &re, licence);
    json!({"differs": re != constructed, "paragraphs_constructed": constructed.len(), "paragraphs_remeasured": re.len(), "form_reasons": sorted(codes)})
}

/// The licence screen on a child's frame lines: a reason outside the frame
/// licence gives `form_frame_<reason>` (its first, in `classify`'s order).
/// Returns (codes, unlicensed reasons as `frame:<reason>`).
fn frame_reasons(text: &str, lines: &[(usize, usize)], licence: &[String]) -> (Vec<String>, Vec<String>) {
    let (mut codes, mut unl) = (Vec::new(), Vec::new());
    for &(a, b) in lines {
        let bad: Vec<&str> = classify_all(&text[a..b]).into_iter().filter(|r| !licence.iter().any(|l| l == r)).collect();
        if let Some(first) = bad.first() {
            codes.push(format!("form_frame_{}", v1_reason(first)));
            unl.extend(bad.iter().map(|r| format!("frame:{r}")));
        }
    }
    (codes, unl)
}

/// The licence screen of one transferred child (amendment 10 item 10.5) on
/// its final document `doc`, where `fin` is the child's generated text (the
/// body for a mirror or donor; the replacement for a span). On the prose
/// path it gives (form reasons, unlicensed reasons); a line-form child is
/// not screened and its would-be unlicensed reasons are returned instead,
/// as (empty, would-be).
struct Licence {
    form: Vec<String>,
    unlicensed: Vec<String>,
    would_be: Vec<String>,
    remeasure: Value,
}

/// `paras`: the document's paragraphs by construction that hold the child's
/// generated text (its body units for a mirror or donor; for a span, the
/// constructed paragraphs over `fin`). `whole` is every constructed
/// paragraph of the child (frame lines included) for a mirror or donor,
/// compared with a re-measurement of the whole text.
#[allow(clippy::too_many_arguments)]
fn licence_screen(t: &Transferred, src: &SourceLayout, conv: &Conventions, raw: &str, doc: &str, fin: (usize, usize), frames: bool, paras: &[(usize, usize)]) -> Licence {
    let (codes, mut unl) = licence_reasons(doc, paras, &src.licence);
    let (fcodes, funl) = if frames { frame_reasons(&t.text, &t.frame_lines, &src.frame_licence) } else { (Vec::new(), Vec::new()) };
    unl.extend(funl);
    let remeasure = if frames {
        // A whole-text child: every constructed unit against the whole text's
        // re-measured paragraphs; the would-be codes over the body range.
        let m = Measure::of(doc);
        let re: Vec<(usize, usize)> = m.paragraphs.iter().map(|p| p.range).collect();
        let over: Vec<(usize, usize)> = m.paragraphs_overlapping(fin.0, fin.1).map(|k| m.paragraphs[k].range).collect();
        let (rc, _) = licence_reasons(doc, &over, &src.licence);
        json!({"differs": re != t.paragraphs, "paragraphs_constructed": t.paragraphs.len(), "paragraphs_remeasured": re.len(), "form_reasons": sorted(rc)})
    } else {
        remeasure(doc, paras, fin, &src.licence)
    };
    if t.record.path == TransferPath::Prose {
        let mut form = codes;
        form.extend(fcodes);
        Licence { form, unlicensed: unl, would_be: Vec::new(), remeasure }
    } else {
        let (_, lines) = licensed_line_reasons(raw, conv, src);
        unl.extend(lines.into_iter().map(|r| format!("line:{r}")));
        Licence { form: Vec::new(), unlicensed: Vec::new(), would_be: sorted(unl), remeasure }
    }
}

/// A whole-text child's body units: its units that are not frame lines.
fn body_units(t: &Transferred) -> Vec<(usize, usize)> {
    t.paragraphs.iter().copied().filter(|r| !t.frame_lines.contains(r)).collect()
}

fn sorted(mut v: Vec<String>) -> Vec<String> {
    v.sort();
    v.dedup();
    v
}

/// The profile v2 step for one generated child: the layout transfer from
/// its source (the whole parent for a mirror, `span = None`; the replaced
/// span's paragraphs and lines for a mixed child, spliced by
/// `generation::assemble`'s rules, `splice`), the licence screen on the
/// prose path (the licence-filtered line reasons; `classify_all` on every
/// paragraph of the final document holding generated text, read by
/// construction (amendment 13 item 13.1: the parent's own boundaries for
/// parent text, the transfer's units for generated text; a re-measurement is
/// recorded as `remeasure` and decides nothing), against the source body's
/// licence; a mirror's frame
/// lines against the frame licence), and the echo screen on the raw child
/// (`echo::screen_item`). `raw` is the cleaned response before
/// normalisation, as the raw twin splices it; `conv` the item's
/// conventions. A spliced item with insertions or several edits uses
/// `item_v2`.
pub fn child_v2(parent: &str, span: Option<(usize, usize)>, conv: &Conventions, raw: &str) -> ChildV2 {
    let edits;
    let child = match span {
        None => echo::Child::Mirror(raw),
        Some(sp) => {
            edits = [echo::Edit { span: sp, raw }];
            echo::Child::Mixed(&edits)
        }
    };
    let verdict = echo::screen_item(parent, conv, child);
    let src = match span {
        None => Some(SourceLayout::whole(parent)),
        Some((s, e)) => SourceLayout::span(parent, s, e),
    };
    let Some(src) = src else {
        let mut reasons = vec!["form_span_outside_parent".to_string()];
        reasons.extend(verdict.reasons());
        let record = json!({"profile": PROFILE_V2, "normaliser": NORM_ID, "layout_measure": MEASURE_ID, "layout_transfer": LAYOUT_TRANSFER_ID,
            "form_screen": FORM_LICENCE_SCREEN_ID, "conventions": conv, "form_reasons": ["form_span_outside_parent"], "echo": verdict.record(),
            "raw_response_sha256": sha256_hex(raw.as_bytes())});
        return ChildV2 { text: String::new(), reasons: sorted(reasons), record };
    };
    let t = transfer(raw, conv, &src);
    let screened = t.record.path == TransferPath::Prose;
    let mut form: Vec<String> = if screened { t.line_reasons.clone() } else { Vec::new() };
    let mut unlicensed: Vec<String> = if screened { t.line_unlicensed.iter().map(|r| format!("line:{r}")).collect() } else { Vec::new() };
    let mut would_be: Vec<String> = Vec::new();
    let mut remeasured = Value::Null;
    if t.text.is_empty() {
        form.push("form_empty".into());
    } else {
        let doc = match span {
            None => Ok((t.text.clone(), t.body.unwrap_or((0, t.text.len())))),
            Some(sp) => splice(parent, sp, &t.text).map(|d| (d, (sp.0, sp.0 + t.text.len()))),
        };
        match doc {
            Ok((d, fin)) => {
                let paras = match span {
                    None => body_units(&t),
                    Some(sp) => overlapping(&constructed_paragraphs(parent, &d, &[Placed { span: sp, sep: None, fin, units: &t.paragraphs }]), fin),
                };
                let l = licence_screen(&t, &src, conv, raw, &d, fin, span.is_none(), &paras);
                form.extend(l.form);
                unlicensed.extend(l.unlicensed);
                would_be = l.would_be;
                remeasured = l.remeasure;
            }
            Err(e) => form.push(format!("form_assembly_error:{e}")),
        }
    }
    let form = sorted(form);
    let mut reasons = form.clone();
    reasons.extend(verdict.reasons());
    let record = json!({"profile": PROFILE_V2, "normaliser": NORM_ID, "layout_measure": MEASURE_ID, "layout_transfer": LAYOUT_TRANSFER_ID,
        "conventions": conv, "source": src.counts, "transfer": t.record, "changes": t.changes, "layout": t.layout,
        "form_screen": FORM_LICENCE_SCREEN_ID, "form_screen_applied": screened, "form_screen_reading": SCREEN_READING, "form_licence": {"body": src.licence, "frame": src.frame_licence},
        "form_reasons": form, "unlicensed_reasons": sorted(unlicensed), "would_be_unlicensed": would_be, "remeasure": remeasured, "echo": verdict.record(),
        "raw_response_sha256": sha256_hex(raw.as_bytes()), "input_sha256": sha256_hex(raw.as_bytes()),
        "output_sha256": sha256_hex(t.text.as_bytes()), "changed": t.text != raw});
    ChildV2 { text: t.text, reasons: sorted(reasons), record }
}

/// One generated edit of a spliced item under profile v2: the parent byte
/// range it replaces (`s == e` for an insertion), the separator assembly
/// puts before an insertion's text, and the raw generated text (before
/// normalisation, as the raw twin splices it).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EditV2<'a> {
    pub span: (usize, usize),
    pub insert_sep: Option<&'a str>,
    pub raw: &'a str,
}

/// A spliced item under profile v2.
#[derive(Clone, Debug)]
pub struct ItemV2 {
    /// Each edit's laid-out replacement, in edit order ("" when its source
    /// is outside the parent).
    pub reps: Vec<String>,
    /// The assembled document, when every edit transferred and assembly
    /// succeeded.
    pub assembled: Option<Assembled>,
    /// Every rejection reason of the item: form (the licence screen, for the
    /// prose-path edits, each against its own licence) and echo (all edits).
    pub reasons: Vec<String>,
    /// Ids, conventions, per-edit sources, transfers and screens, the echo
    /// verdict and digests; no text. Callers add the parent id.
    pub record: Value,
}

/// One edit's slot in an item, for the layout in parent order (amendment 29
/// item 29.1): its splice and, when known, its raw text. A slot with no raw
/// text (a sibling not yet accepted) keeps the parent's text; its place
/// still decides whether the edit before it shares a line with it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Slot<'a> {
    pub span: (usize, usize),
    pub insert_sep: Option<&'a str>,
    pub raw: Option<&'a str>,
}

/// One edit laid out in its item: its source, with the columns of the item
/// assembled so far, and its transfer.
#[derive(Clone, Debug)]
pub struct LaidEdit {
    pub src: SourceLayout,
    pub transferred: Transferred,
}

/// Lays out the known edits of an item one at a time in the order assembly
/// places them in the parent (`assemble_item`'s order; amendment 29 item
/// 29.1). Each edit takes its own source (`SourceLayout::span`, or
/// `SourceLayout::insertion` at the point with its separator) with two
/// columns measured on the item assembled so far:
/// * **its first line's column**: the columns of the document line's text
///   before it, when that line holds an earlier edit's laid-out text
///   (`SourceLayout::sibling_column`; the transfer then starts a wrapped
///   first unit on a new line when its first word has no room); otherwise
///   the parent's column, unchanged;
/// * **its last line's tail**: when the next edit (known or not) starts on
///   the same document line, the columns of the parent text between the
///   two edits only; otherwise the parent text up to the line's body end,
///   unchanged.
///
/// Each edit then takes amendment 38's split (item 38.1, as amendment 39
/// item 39.1 reads it; `transfer`'s module notes): its document lines are
/// read from the item assembled so far before it, the earlier edits'
/// wrapped units with their W (`LineContext::widths`), and, when the next
/// edit starts on its last line, the parent text up to that edit only, or,
/// when the next edit is an insertion whose separator breaks the line, the
/// parent text up to its point and the separator up to that break
/// (`SourceLayout::line`), with break points in its own text. A later
/// sibling, known or not, never changes an earlier edit's text.
///
/// Returns one entry per slot, in slot order: None for a slot with no raw
/// text or a source outside the parent. Once an edit is outside the parent
/// or out of order (assembly then fails), the later edits take their
/// parent's columns and are split as each alone.
pub fn lay_out_item(parent: &str, conv: &Conventions, slots: &[Slot]) -> Vec<Option<LaidEdit>> {
    let mut order: Vec<usize> = (0..slots.len()).collect();
    order.sort_by_key(|&i| slots[i].span);
    let mut out: Vec<Option<LaidEdit>> = vec![None; slots.len()];
    // the item assembled so far (known edits only), the parent bytes it has
    // consumed and where the last known edit's text ends in it
    let mut doc = String::new();
    let mut cur = 0usize;
    let mut last_end: Option<usize> = None;
    // the known edits' wrapped units in `doc`, each with its edit's W
    // (amendment 38's W on a shared line, amendment 39 item 39.1)
    let mut wrapped: Vec<((usize, usize), usize)> = Vec::new();
    let mut track = true;
    for (k, &i) in order.iter().enumerate() {
        let sl = slots[i];
        let Some(raw) = sl.raw else { continue };
        let (s, e) = sl.span;
        let src = if s == e { SourceLayout::insertion(parent, s, sl.insert_sep.unwrap_or("")) } else { SourceLayout::span(parent, s, e) };
        let Some(mut src) = src.filter(|_| s <= e && parent.get(s..e).is_some()) else {
            track = false;
            continue;
        };
        if track {
            if let Some(&j) = order.get(k + 1) {
                let (s2, e2) = slots[j].span;
                let breaks = s2 == e2 && slots[j].insert_sep.is_some_and(|x| x.contains('\n'));
                if let Some(between) = parent.get(e..s2).filter(|b| !b.contains('\n')) {
                    if !breaks {
                        src.tail_column = between.chars().count();
                    }
                    // amendment 38's line after the edit is the assembled
                    // line, read up to the next edit (amendment 39 item 39.1):
                    // the parent text up to it, and for an insertion whose
                    // separator breaks the line, the separator up to that
                    // break (item 29.1's tail stays the parent's there)
                    let sep = slots[j].insert_sep.filter(|_| breaks).map_or("", |x| &x[..x.find('\n').unwrap_or(x.len())]);
                    if let Some(l) = src.line.as_mut() {
                        l.after = format!("{between}{sep}");
                    }
                }
            }
            match parent.get(cur..s) {
                Some(seg) => {
                    doc.push_str(seg);
                    if s == e {
                        doc.push_str(sl.insert_sep.unwrap_or(""));
                    }
                    if last_end.is_some_and(|le| !doc[le..].contains('\n')) {
                        let ls = doc.rfind('\n').map_or(0, |x| x + 1);
                        src.column = doc[ls..].chars().count();
                        src.sibling_column = true;
                        // amendment 38: the document line before the edit,
                        // earlier edits' laid-out text included
                        let after = src.line.take().map(|l| l.after).unwrap_or_default();
                        let widths = wrapped.iter().filter(|r| r.0.1 > ls).map(|&((a, b), w)| ((a.max(ls) - ls, b - ls), w)).collect();
                        src.line = Some(LineContext { before: doc[ls..].to_string(), after, widths });
                    }
                }
                None => track = false,
            }
        }
        let t = transfer(raw, conv, &src);
        if track {
            if let Some(w) = src.width {
                let at = doc.len();
                wrapped.extend(t.paragraphs.iter().zip(&t.unit_wrapped).filter(|x| *x.1).map(|(&(a, b), _)| ((at + a, at + b), w)));
            }
            doc.push_str(&t.text);
            last_end = Some(doc.len());
            cur = e;
        }
        out[i] = Some(LaidEdit { src, transferred: t });
    }
    out
}

/// The applications of amendment 38's split recorded in a profile v2
/// record (`child_v2`, `item_v2`, `edit_in_item_v2` or `donor_child_v2`):
/// its `transfer.document_line_splits` and each of its `edits`'. This is
/// every edit's count: an `item_v2` record over a whole item (an item's last
/// request) holds its earlier siblings' edits too, so a summary over
/// requests counts such a record's own edit only (`edit_line_splits`).
pub fn document_line_splits(record: &Value) -> u64 {
    one_line_splits(record) + record["edits"].as_array().map_or(0, |es| es.iter().map(one_line_splits).sum())
}

fn one_line_splits(r: &Value) -> u64 {
    r["transfer"]["document_line_splits"].as_u64().unwrap_or(0)
}

/// The applications of amendment 38's split in one edit of a profile v2
/// record: `edits[index]`'s, or, for a record with no `edits`, its own
/// (`child_v2` or `donor_child_v2`). Each application is counted once when a
/// summary adds this over an item's requests with each request's index.
pub fn edit_line_splits(record: &Value, index: usize) -> u64 {
    match record["edits"].as_array() {
        Some(es) => es.get(index).map_or(0, one_line_splits),
        None => one_line_splits(record),
    }
}

/// The profile v2 step for a spliced item with any number of edits,
/// insertions included (`EditV2`): the edits are laid out in parent order
/// (`lay_out_item`; amendment 29 item 29.1), each from its own source
/// (`SourceLayout::span`, or `SourceLayout::insertion` at the point with its
/// separator) with the columns of the item assembled so far; the
/// replacements are assembled by the spliced generator's
/// rules (`assemble_item`); the licence screen applies to each prose-path
/// edit with its own licence (amendment 7 item 7.9): its line reasons and
/// `classify_all` on every paragraph of the assembled item holding its text,
/// read by construction over all the item's edits (amendment 13 item 13.1);
/// and the echo screen compares every edit, insertions with the empty span
/// (`echo::screen_item`). An item with no edit fails closed
/// (`echo_mixed_no_edits`).
pub fn item_v2(parent: &str, conv: &Conventions, edits: &[EditV2]) -> ItemV2 {
    let slots: Vec<Slot> = edits.iter().map(|e| Slot { span: e.span, insert_sep: e.insert_sep, raw: Some(e.raw) }).collect();
    let laid = lay_out_item(parent, conv, &slots);
    item_v2_laid(parent, conv, edits, laid)
}

/// The step for one edit of an item judged before its siblings
/// (`FormPost::of_item`'s earlier requests): `item_v2` over that edit
/// alone, its layout taken in its item (`lay_out_item` over `slots`, every
/// edit of the item with the raw texts known so far). The edit at `index`
/// must have its raw text; without one the edit is laid out from "".
/// With no sibling known on its lines this is `item_v2` over the edit alone
/// exactly.
pub fn edit_in_item_v2(parent: &str, conv: &Conventions, slots: &[Slot], index: usize) -> ItemV2 {
    let own = slots[index];
    let mut slots = slots.to_vec();
    slots[index].raw = Some(own.raw.unwrap_or(""));
    let mut laid = lay_out_item(parent, conv, &slots);
    let e = EditV2 { span: own.span, insert_sep: own.insert_sep, raw: own.raw.unwrap_or("") };
    item_v2_laid(parent, conv, &[e], vec![laid[index].take()])
}

/// `item_v2`'s screens and assembly over edits already laid out (`laid`,
/// one per edit: None when its source is outside the parent).
fn item_v2_laid(parent: &str, conv: &Conventions, edits: &[EditV2], laid: Vec<Option<LaidEdit>>) -> ItemV2 {
    let echo_edits: Vec<echo::Edit> = edits.iter().map(|e| echo::Edit { span: e.span, raw: e.raw }).collect();
    let verdict = echo::screen_item(parent, conv, echo::Child::Mixed(&echo_edits));
    let mut form: Vec<String> = Vec::new();
    let mut reps: Vec<String> = Vec::new();
    let mut licences: Vec<Option<Vec<String>>> = Vec::new();
    let mut units: Vec<Vec<(usize, usize)>> = Vec::new();
    let mut recs: Vec<Value> = Vec::new();
    let mut outside = false;
    for (e, laid) in edits.iter().zip(laid) {
        let (s, t) = e.span;
        let mut rec = json!({"span": [s, t], "insertion": s == t, "insert_sep": e.insert_sep.map(escape_ws), "raw_response_sha256": sha256_hex(e.raw.as_bytes())});
        let Some(LaidEdit { src, transferred: tr }) = laid else {
            outside = true;
            form.push("form_span_outside_parent".into());
            rec["form_reasons"] = json!(["form_span_outside_parent"]);
            reps.push(String::new());
            licences.push(None);
            units.push(Vec::new());
            recs.push(rec);
            continue;
        };
        let on = tr.record.path == TransferPath::Prose;
        let mut own: Vec<String> = if on { tr.line_reasons.clone() } else { Vec::new() };
        if tr.text.is_empty() {
            own.push("form_empty".into());
        }
        form.extend(own.iter().cloned());
        rec["source"] = json!(src.counts);
        rec["transfer"] = json!(tr.record);
        rec["changes"] = json!(tr.changes);
        rec["layout"] = json!(tr.layout);
        rec["form_screen"] = json!(FORM_LICENCE_SCREEN_ID);
        rec["form_screen_applied"] = json!(on);
        rec["form_licence"] = json!({"body": src.licence, "frame": src.frame_licence});
        rec["form_reasons"] = json!(sorted(own));
        rec["unlicensed_reasons"] = json!(sorted(if on { tr.line_unlicensed.iter().map(|r| format!("line:{r}")).collect() } else { Vec::new() }));
        rec["output_sha256"] = json!(sha256_hex(tr.text.as_bytes()));
        units.push(tr.paragraphs.clone());
        reps.push(tr.text);
        licences.push(on.then(|| src.licence.clone()));
        recs.push(rec);
    }
    if edits.is_empty() {
        form.push("form_empty".into());
    }
    let mut assembled = None;
    if !outside && !edits.is_empty() && reps.iter().all(|r| !r.is_empty()) {
        let parts: Vec<Part> = edits.iter().map(|e| Part { span: e.span, insert_sep: e.insert_sep }).collect();
        let rs: Vec<&str> = reps.iter().map(String::as_str).collect();
        match assemble_item(parent, &parts, &rs) {
            Ok(a) => {
                // each replacement's range in the document: its final span
                // and a leading line break and lead (amendment 29 item 29.1)
                let fins: Vec<(usize, usize)> = (0..edits.len()).map(|i| (a.edit_spans[i].0 - replacement_lead(&reps[i]).unwrap_or(0), a.edit_spans[i].1)).collect();
                let placed: Vec<Placed> = (0..edits.len()).map(|i| Placed { span: edits[i].span, sep: edits[i].insert_sep, fin: fins[i], units: &units[i] }).collect();
                let paras = constructed_paragraphs(parent, &a.text, &placed);
                for (i, lic) in licences.iter().enumerate() {
                    if let Some(l) = lic {
                        let own = overlapping(&paras, fins[i]);
                        let (codes, unl) = licence_reasons(&a.text, &own, l);
                        recs[i]["remeasure"] = remeasure(&a.text, &own, fins[i], l);
                        form.extend(codes);
                        if !unl.is_empty() {
                            let mut u: Vec<String> = serde_json::from_value(recs[i]["unlicensed_reasons"].clone()).unwrap_or_default();
                            u.extend(unl);
                            recs[i]["unlicensed_reasons"] = json!(sorted(u));
                        }
                    }
                }
                assembled = Some(a);
            }
            Err(e) => form.push(format!("form_assembly_error:{e}")),
        }
    }
    let form = sorted(form);
    let mut reasons = form.clone();
    reasons.extend(verdict.reasons());
    let record = json!({"profile": PROFILE_V2, "normaliser": NORM_ID, "layout_measure": MEASURE_ID, "layout_transfer": LAYOUT_TRANSFER_ID,
        "form_screen": FORM_LICENCE_SCREEN_ID, "form_screen_reading": SCREEN_READING, "conventions": conv, "edits": recs, "form_reasons": form, "echo": verdict.record(),
        "output_sha256": assembled.as_ref().map(|a| sha256_hex(a.text.as_bytes())),
        "final_spans": assembled.as_ref().map(|a| a.edit_spans.clone())});
    ItemV2 { reps, assembled, reasons: sorted(reasons), record }
}

/// The profile v2 step for a parentless row with a layout donor (§6.2:
/// stage-3 legacy rows and captures): the layout transfer from the donor's
/// layout (`SourceLayout::whole` of a keyed same-domain Train-2 human, plan
/// key x = `"layout_donor"`; its properties only, frames included), then the
/// licence screen on the prose path (the line reasons, `classify_all` on
/// every body paragraph of the laid-out child against the donor's body
/// licence, and its frame lines against the frame licence). There is no
/// parent span to echo, so no echo comparison runs; the record says so. A
/// span source fails closed (`form_donor_is_a_span`). Callers add the row id
/// and the donor id.
pub fn donor_child_v2(donor: &SourceLayout, conv: &Conventions, raw: &str) -> ChildV2 {
    let echo = json!({"id": ECHO_SCREEN_ID, "applied": false, "reason": "no_recorded_parent"});
    if donor.mixed {
        let record = json!({"profile": PROFILE_V2, "normaliser": NORM_ID, "layout_measure": MEASURE_ID, "layout_transfer": LAYOUT_TRANSFER_ID, "layout_source": "donor",
            "form_screen": FORM_LICENCE_SCREEN_ID, "conventions": conv, "form_reasons": ["form_donor_is_a_span"], "echo": echo, "raw_response_sha256": sha256_hex(raw.as_bytes())});
        return ChildV2 { text: String::new(), reasons: vec!["form_donor_is_a_span".into()], record };
    }
    let t = transfer(raw, conv, donor);
    let screened = t.record.path == TransferPath::Prose;
    let mut form: Vec<String> = if screened { t.line_reasons.clone() } else { Vec::new() };
    let mut unlicensed: Vec<String> = if screened { t.line_unlicensed.iter().map(|r| format!("line:{r}")).collect() } else { Vec::new() };
    let mut would_be = Vec::new();
    let mut remeasured = Value::Null;
    if t.text.is_empty() {
        form.push("form_empty".into());
    } else {
        let l = licence_screen(&t, donor, conv, raw, &t.text, t.body.unwrap_or((0, t.text.len())), true, &body_units(&t));
        form.extend(l.form);
        unlicensed.extend(l.unlicensed);
        would_be = l.would_be;
        remeasured = l.remeasure;
    }
    let form = sorted(form);
    let record = json!({"profile": PROFILE_V2, "normaliser": NORM_ID, "layout_measure": MEASURE_ID, "layout_transfer": LAYOUT_TRANSFER_ID, "layout_source": "donor",
        "conventions": conv, "source": donor.counts, "transfer": t.record, "changes": t.changes, "layout": t.layout,
        "form_screen": FORM_LICENCE_SCREEN_ID, "form_screen_applied": screened, "form_screen_reading": SCREEN_READING, "form_licence": {"body": donor.licence, "frame": donor.frame_licence},
        "form_reasons": form, "unlicensed_reasons": sorted(unlicensed), "would_be_unlicensed": would_be, "remeasure": remeasured, "echo": echo,
        "raw_response_sha256": sha256_hex(raw.as_bytes()), "input_sha256": sha256_hex(raw.as_bytes()),
        "output_sha256": sha256_hex(t.text.as_bytes()), "changed": t.text != raw});
    ChildV2 { text: t.text, reasons: form, record }
}
