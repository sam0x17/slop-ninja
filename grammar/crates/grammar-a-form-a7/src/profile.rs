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
//! * `cc_cohort_profile_v2` (protocol A2 §6.2, as amended by amendment 6):
//!   v1 with the parent-conditioned layout transfer `layout_transfer_v1` in
//!   place of v1's layout step, the form screen applied exactly when the
//!   source passes it, the echo screen `cc_echo_screen_v1` on every child
//!   with a parent, and keyed convention draws from the prior of the child's
//!   own population and domain (`PriorV2`, `resolve_v2`) under the
//!   population's own seed (`norm_prior_seed`). Entry points: `child_v2` (a
//!   mirror, or one replaced span), `item_v2` (a spliced item: any number of
//!   edits, insertions included, screened on the assembled item) and
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
use crate::measure::{Measure, escape_ws};
use crate::screen::paragraph_screen;
use crate::text;
use crate::transfer::{LAYOUT_TRANSFER_ID, SourceLayout, transfer};
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
    /// Protocol A2 §6.2: v1 with the layout transfer, the form screen iff
    /// the source passes it, the echo screen and per-population x domain
    /// priors.
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
    /// Whether the profile lays children out with `layout_transfer_v1`
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
            return json!({"profile": self.id(), "normaliser": NORM_ID, "layout_transfer": LAYOUT_TRANSFER_ID, "echo_screen": ECHO_SCREEN_ID,
                "length_window": {"id": LENGTH_WINDOW_ID, "min_words": w.0, "max_words": w.1, "unit": "whitespace words of the final document"},
                "form_screen": "applied to a child exactly when its source passes it",
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

/// The key seed of population `population`'s convention prior (amendment 6
/// item 6.8): V21's plan key with x = `"norm_prior"` and d.id = the
/// population id, as lowercase hex. Each population has its own seed by
/// construction, and the seed is fixed before any draw uses it.
pub fn norm_prior_seed(population: &str) -> String {
    sha256_hex(format!("{R2_PLAN_KEY_PREFIX}{NORM_PRIOR_KEY}:{population}").as_bytes())
}

// ------------------------------------------------------------------ v2 child

/// One child under profile v2.
#[derive(Clone, Debug)]
pub struct ChildV2 {
    /// The laid-out child (for a span, the trimmed replacement).
    pub text: String,
    /// Every rejection reason: form (when the screen applies) and echo.
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

/// `parent[..s] + rep + parent[e..]` under `generation::assemble`'s splice
/// rules (a non-empty span on character and token boundaries, a non-empty
/// trimmed replacement), with its error messages.
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
    if rep.is_empty() || rep.trim() != rep {
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
/// non-empty and trimmed; the human characters, the final boundaries and the
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
        if r.is_empty() || r.trim_matches(py_space) != r {
            return Err("replacement layout");
        }
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
        let fs = out.len();
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

/// The form screen on an assembled document: `screen::classify` on every
/// paragraph (read with the v2 paragraph rule, `measure`) that overlaps one
/// of `spans`, the final spans of the edits whose source passes the screen.
fn paragraph_reasons(doc: &str, spans: &[(usize, usize)]) -> Vec<String> {
    if spans.is_empty() {
        return Vec::new();
    }
    let m = Measure::of(doc);
    let mut out = Vec::new();
    for &(a, b) in spans {
        for k in m.paragraphs_overlapping(a, b) {
            if let Some(r) = m.paragraphs[k].reason {
                out.push(format!("form_paragraph_{r}"));
            }
        }
    }
    out
}

fn sorted(mut v: Vec<String>) -> Vec<String> {
    v.sort();
    v.dedup();
    v
}

/// The profile v2 step for one generated child: `layout_transfer_v1` from
/// its source (the whole parent for a mirror, `span = None`; the replaced
/// span's paragraphs and lines for a mixed child, spliced by
/// `generation::assemble`'s rules, `splice`), the form screen when the
/// source passes it (the line reasons, and `classify` on every paragraph of
/// the final document holding generated text, read with the v2 paragraph
/// rule), and the echo screen on the raw child (`echo::screen_item`). `raw`
/// is the cleaned response before normalisation, as the raw twin splices
/// it; `conv` the item's conventions. A spliced item with insertions or
/// several edits uses `item_v2`.
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
        let record = json!({"profile": PROFILE_V2, "normaliser": NORM_ID, "layout_transfer": LAYOUT_TRANSFER_ID, "conventions": conv,
            "form_reasons": ["form_span_outside_parent"], "echo": verdict.record(), "raw_response_sha256": sha256_hex(raw.as_bytes())});
        return ChildV2 { text: String::new(), reasons: sorted(reasons), record };
    };
    let t = transfer(raw, conv, &src);
    let screened = src.passes_screen;
    let mut form: Vec<String> = if screened { t.line_reasons.clone() } else { Vec::new() };
    if t.text.is_empty() {
        form.push("form_empty".into());
    } else {
        let doc = match span {
            None => Ok((t.text.clone(), (0usize, t.text.len()))),
            Some(sp) => splice(parent, sp, &t.text).map(|d| (d, (sp.0, sp.0 + t.text.len()))),
        };
        match doc {
            Ok((d, fin)) if screened => form.extend(paragraph_reasons(&d, &[fin])),
            Ok(_) => {}
            Err(e) => form.push(format!("form_assembly_error:{e}")),
        }
    }
    let form = sorted(form);
    let mut reasons = form.clone();
    reasons.extend(verdict.reasons());
    let record = json!({"profile": PROFILE_V2, "normaliser": NORM_ID, "layout_transfer": LAYOUT_TRANSFER_ID,
        "conventions": conv, "source": src.counts, "transfer": t.record, "changes": t.changes, "layout": t.layout,
        "form_screen_applied": screened, "form_reasons": form, "echo": verdict.record(),
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
    /// Every rejection reason of the item: form (for the edits whose source
    /// passes the screen) and echo (all edits).
    pub reasons: Vec<String>,
    /// Ids, conventions, per-edit sources, transfers and screens, the echo
    /// verdict and digests; no text. Callers add the parent id.
    pub record: Value,
}

/// The profile v2 step for a spliced item with any number of edits,
/// insertions included (`EditV2`): each edit is laid out from its own source
/// (`SourceLayout::span`, or `SourceLayout::insertion` at the point with its
/// separator); the replacements are assembled by the spliced generator's
/// rules (`assemble_item`); the form screen applies to each edit whose
/// source passes it: its line reasons and `classify` on every paragraph of
/// the assembled item holding its text; and the echo screen compares every
/// edit, insertions with the empty span (`echo::screen_item`). An item with
/// no edit fails closed (`echo_mixed_no_edits`).
pub fn item_v2(parent: &str, conv: &Conventions, edits: &[EditV2]) -> ItemV2 {
    let echo_edits: Vec<echo::Edit> = edits.iter().map(|e| echo::Edit { span: e.span, raw: e.raw }).collect();
    let verdict = echo::screen_item(parent, conv, echo::Child::Mixed(&echo_edits));
    let mut form: Vec<String> = Vec::new();
    let mut reps: Vec<String> = Vec::new();
    let mut screened: Vec<bool> = Vec::new();
    let mut recs: Vec<Value> = Vec::new();
    let mut outside = false;
    for e in edits {
        let (s, t) = e.span;
        let src = if s == t { SourceLayout::insertion(parent, s, e.insert_sep.unwrap_or("")) } else { SourceLayout::span(parent, s, t) };
        let mut rec = json!({"span": [s, t], "insertion": s == t, "insert_sep": e.insert_sep.map(escape_ws), "raw_response_sha256": sha256_hex(e.raw.as_bytes())});
        let Some(src) = src.filter(|_| s <= t && parent.get(s..t).is_some()) else {
            outside = true;
            form.push("form_span_outside_parent".into());
            rec["form_reasons"] = json!(["form_span_outside_parent"]);
            reps.push(String::new());
            screened.push(false);
            recs.push(rec);
            continue;
        };
        let tr = transfer(e.raw, conv, &src);
        let on = src.passes_screen;
        let mut own: Vec<String> = if on { tr.line_reasons.clone() } else { Vec::new() };
        if tr.text.is_empty() {
            own.push("form_empty".into());
        }
        form.extend(own.iter().cloned());
        rec["source"] = json!(src.counts);
        rec["transfer"] = json!(tr.record);
        rec["changes"] = json!(tr.changes);
        rec["layout"] = json!(tr.layout);
        rec["form_screen_applied"] = json!(on);
        rec["form_reasons"] = json!(sorted(own));
        rec["output_sha256"] = json!(sha256_hex(tr.text.as_bytes()));
        reps.push(tr.text);
        screened.push(on);
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
                let fins: Vec<(usize, usize)> = a.edit_spans.iter().zip(&screened).filter(|x| *x.1).map(|x| *x.0).collect();
                form.extend(paragraph_reasons(&a.text, &fins));
                assembled = Some(a);
            }
            Err(e) => form.push(format!("form_assembly_error:{e}")),
        }
    }
    let form = sorted(form);
    let mut reasons = form.clone();
    reasons.extend(verdict.reasons());
    let record = json!({"profile": PROFILE_V2, "normaliser": NORM_ID, "layout_transfer": LAYOUT_TRANSFER_ID, "conventions": conv,
        "edits": recs, "form_reasons": form, "echo": verdict.record(),
        "output_sha256": assembled.as_ref().map(|a| sha256_hex(a.text.as_bytes())),
        "final_spans": assembled.as_ref().map(|a| a.edit_spans.clone())});
    ItemV2 { reps, assembled, reasons: sorted(reasons), record }
}

/// The profile v2 step for a parentless row with a layout donor (§6.2:
/// stage-3 legacy rows and captures): `layout_transfer_v1` from the donor's
/// layout (`SourceLayout::whole` of a keyed same-domain Train-2 human, plan
/// key x = `"layout_donor"`; its properties only), then the form screen when
/// the donor passes it (the line reasons and `classify` on every paragraph
/// of the laid-out child). There is no parent span to echo, so no echo
/// comparison runs; the record says so. A span source fails closed
/// (`form_donor_is_a_span`). Callers add the row id and the donor id.
pub fn donor_child_v2(donor: &SourceLayout, conv: &Conventions, raw: &str) -> ChildV2 {
    let echo = json!({"id": ECHO_SCREEN_ID, "applied": false, "reason": "no_recorded_parent"});
    if donor.mixed {
        let record = json!({"profile": PROFILE_V2, "normaliser": NORM_ID, "layout_transfer": LAYOUT_TRANSFER_ID, "layout_source": "donor",
            "conventions": conv, "form_reasons": ["form_donor_is_a_span"], "echo": echo, "raw_response_sha256": sha256_hex(raw.as_bytes())});
        return ChildV2 { text: String::new(), reasons: vec!["form_donor_is_a_span".into()], record };
    }
    let t = transfer(raw, conv, donor);
    let screened = donor.passes_screen;
    let mut form: Vec<String> = if screened { t.line_reasons.clone() } else { Vec::new() };
    if t.text.is_empty() {
        form.push("form_empty".into());
    } else if screened {
        form.extend(paragraph_reasons(&t.text, &[(0, t.text.len())]));
    }
    let form = sorted(form);
    let record = json!({"profile": PROFILE_V2, "normaliser": NORM_ID, "layout_transfer": LAYOUT_TRANSFER_ID, "layout_source": "donor",
        "conventions": conv, "source": donor.counts, "transfer": t.record, "changes": t.changes, "layout": t.layout,
        "form_screen_applied": screened, "form_reasons": form, "echo": echo,
        "raw_response_sha256": sha256_hex(raw.as_bytes()), "input_sha256": sha256_hex(raw.as_bytes()),
        "output_sha256": sha256_hex(t.text.as_bytes()), "changed": t.text != raw});
    ChildV2 { text: t.text, reasons: form, record }
}
