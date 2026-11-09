//! The virtual trivial policies: content-free span
//! placements computed from the anchor's document outputs alone, which bound
//! what a head with no localization signal reaches. A gate whose measured
//! conditions some trivial policy passes cannot tell localization from
//! placement by position or document share, so the per-cohort anchor check
//! waives the measured conditions when one does ([`TrivialSet::any_passes`]).
//!
//! - `Whole(τ)`: the whole document when the anchor's predicted share is at
//!   least τ;
//! - `Template(position, k, τ)`: a fixed run of `k`% of the document's
//!   sentences at its start, middle or end (rounded half up, at least one and
//!   leaving one, as `sentence_splice_v1` selects), when the share is at
//!   least τ;
//! - `Sized(position, τ)`: the same, sized to `round(share × sentences)`.
//!
//! τ runs over a 0.05 grid from 0 to 1. Sentences are the caller's (the
//! public splitter the cohort's text generation uses), as byte ranges. A
//! placement under the 24-word floor is never decoded, as no admissible
//! decoder emits one. Where the anchor's prediction failed, a policy decodes
//! nothing on a mixed document and counts as flagged on a human one, as the
//! anchor itself does.
use crate::{Agg, FLOOR_WORDS, Margins, PPB, Population, Unpaired, f1_ppb, is_space, measured};

/// Where a template run sits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Position {
    Start,
    Middle,
    End,
}

impl Position {
    pub const ALL: &'static [Position] = &[Position::Start, Position::Middle, Position::End];

    fn code(self) -> &'static str {
        match self {
            Self::Start => "start",
            Self::Middle => "middle",
            Self::End => "end",
        }
    }
}

/// One trivial policy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Policy {
    Whole { tau_ppb: u32 },
    Template { position: Position, percent: u8, tau_ppb: u32 },
    Sized { position: Position, tau_ppb: u32 },
}

/// The τ grid step, ppb.
pub const TAU_STEP_PPB: u32 = PPB as u32 / 20;
/// The template run sizes, % of the sentences: `sentence_splice_v1`'s classes.
pub const PERCENTS: &[u8] = &[10, 40, 80];

impl Policy {
    /// A content-free name, for reports.
    pub fn name(&self) -> String {
        let t = |tau: u32| format!("{:.2}", f64::from(tau) / PPB as f64);
        match self {
            Self::Whole { tau_ppb } => format!("whole({})", t(*tau_ppb)),
            Self::Template { position, percent, tau_ppb } => {
                format!("template({},{},{})", position.code(), percent, t(*tau_ppb))
            }
            Self::Sized { position, tau_ppb } => format!("sized({},{})", position.code(), t(*tau_ppb)),
        }
    }

    fn tau(&self) -> u32 {
        match *self {
            Self::Whole { tau_ppb } | Self::Template { tau_ppb, .. } | Self::Sized { tau_ppb, .. } => tau_ppb,
        }
    }
}

/// Every policy: `Whole` on the τ grid, `Template` for each position and
/// size on it, `Sized`
/// for each position on it.
pub fn grid() -> Vec<Policy> {
    let taus: Vec<u32> = (0..=PPB as u32 / TAU_STEP_PPB).map(|i| i * TAU_STEP_PPB).collect();
    let mut out = Vec::with_capacity(taus.len() * (1 + Position::ALL.len() * (PERCENTS.len() + 1)));
    for &tau_ppb in &taus {
        out.push(Policy::Whole { tau_ppb });
        for &position in Position::ALL {
            for &percent in PERCENTS {
                out.push(Policy::Template { position, percent, tau_ppb });
            }
            out.push(Policy::Sized { position, tau_ppb });
        }
    }
    out
}

/// Cumulative counts at one byte position: non-space characters, those in
/// the gold run, word starts, and whether the position splits a word.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Mark {
    ns: u64,
    g: u64,
    word_starts: u32,
    inside_word: bool,
}

/// One document's geometry: its totals and the marks at each sentence's start
/// and end, so a run of whole sentences is counted in O(1).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Geometry {
    pub population: Population,
    total: u64,
    g: u64,
    words: u32,
    /// Per sentence: the marks at its start and its end.
    marks: Vec<(Mark, Mark)>,
}

impl Geometry {
    /// Counts `text` against `gold` and `sentences` (sorted, disjoint byte
    /// ranges inside the text) in one pass.
    pub fn of(text: &str, population: Population, gold: Option<(u32, u32)>, sentences: &[(usize, usize)]) -> Self {
        let mut q: Vec<usize> = Vec::with_capacity(2 * sentences.len());
        for &(a, b) in sentences {
            q.push(a.min(text.len()));
            q.push(b.min(text.len()));
        }
        q.sort_unstable();
        q.dedup();
        let mut at_q: Vec<Mark> = Vec::with_capacity(q.len());
        let mut cur = Mark::default();
        let mut prev_word = false;
        let mut qi = 0usize;
        for (b, c) in text.char_indices() {
            while qi < q.len() && q[qi] <= b {
                at_q.push(Mark { inside_word: prev_word && !is_space(c), ..cur });
                qi += 1;
            }
            let w = !is_space(c);
            if w {
                cur.ns += 1;
                if gold.is_some_and(|(x, y)| (b as u32) >= x && (b as u32) < y) {
                    cur.g += 1;
                }
                if !prev_word {
                    cur.word_starts += 1;
                }
            }
            prev_word = w;
        }
        while qi < q.len() {
            at_q.push(Mark { inside_word: false, ..cur });
            qi += 1;
        }
        let mark = |p: usize| -> Mark {
            let p = p.min(text.len());
            match q.binary_search(&p) {
                Ok(i) => at_q.get(i).copied().unwrap_or_default(),
                Err(_) => Mark::default(),
            }
        };
        let marks = sentences.iter().map(|&(a, b)| (mark(a), mark(b))).collect();
        Geometry { population, total: cur.ns, g: cur.g, words: cur.word_starts, marks }
    }

    pub fn sentences(&self) -> usize {
        self.marks.len()
    }

    /// `(|P|, |P ∩ G|, words)` of sentences `first..=last`.
    fn run(&self, first: usize, last: usize) -> Option<(u64, u64, u32)> {
        let (s, _) = self.marks.get(first)?;
        let (_, e) = self.marks.get(last)?;
        let words = e.word_starts.saturating_sub(s.word_starts) + u32::from(s.inside_word);
        Some((e.ns.saturating_sub(s.ns), e.g.saturating_sub(s.g), words))
    }

    /// `(|P|, |P ∩ G|)` of `policy` given the anchor's predicted share, or
    /// `None` when it decodes nothing.
    fn place(&self, policy: &Policy, share_ppb: u32) -> Option<(u64, u64)> {
        if share_ppb < policy.tau() {
            return None;
        }
        let s = self.sentences();
        let run = |k: usize, position: Position| -> Option<(u64, u64, u32)> {
            if s == 0 || k == 0 {
                return None;
            }
            let k = k.min(s);
            let first = match position {
                Position::Start => 0,
                Position::Middle => (s - k) / 2,
                Position::End => s - k,
            };
            self.run(first, first + k - 1)
        };
        let (p, pg, words) = match *policy {
            Policy::Whole { .. } => (self.total, self.g, self.words),
            Policy::Template { position, percent, .. } => {
                if s < 2 {
                    return None;
                }
                let k = ((s as u64 * u64::from(percent) + 50) / 100).clamp(1, s as u64 - 1) as usize;
                run(k, position)?
            }
            Policy::Sized { position, .. } => {
                let k = ((u128::from(share_ppb) * s as u128 + u128::from(PPB) / 2) / u128::from(PPB)) as usize;
                run(k, position)?
            }
        };
        (words >= FLOOR_WORDS && p > 0).then_some((p, pg))
    }
}

/// The policies' sums over a cohort's batch, in [`grid`] order.
#[derive(Clone, Debug)]
pub struct TrivialSet {
    pub policies: Vec<Policy>,
    pub aggs: Vec<Agg>,
}

impl Default for TrivialSet {
    fn default() -> Self {
        let policies = grid();
        let aggs = vec![Agg::default(); policies.len()];
        TrivialSet { policies, aggs }
    }
}

impl TrivialSet {
    /// Adds one batch document, given the anchor's predicted share (`None`
    /// when its prediction failed).
    pub fn add(&mut self, geo: &Geometry, share_ppb: Option<u32>) {
        for (policy, agg) in self.policies.iter().zip(self.aggs.iter_mut()) {
            agg.docs += 1;
            let placed = share_ppb.and_then(|s| geo.place(policy, s));
            match geo.population {
                Population::InScope => {
                    agg.in_scope += 1;
                    agg.human += geo.total - geo.g;
                    if let Some((p, pg)) = placed {
                        agg.f1_sum += u64::from(f1_ppb(pg, p, geo.g));
                        agg.fp_human += p - pg;
                        agg.spans += 1;
                    }
                }
                Population::Human => {
                    agg.humans += 1;
                    agg.flagged += u32::from(share_ppb.is_none() || placed.is_some());
                    agg.spans += u64::from(placed.is_some());
                }
                Population::Reported => {}
            }
        }
    }

    /// Whether any policy passes Q1, QC and QH against the anchor.
    pub fn any_passes(&self, m: &Margins, anchor: &Agg) -> Result<bool, Unpaired> {
        for a in &self.aggs {
            if measured(m, a, anchor)?.is_none() {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// The policy with the highest mean F1 among those whose in-context rate
    /// is at most `max_in_context_ppb` (reported).
    pub fn best_f1_within(&self, max_in_context_ppb: u64) -> Option<(Policy, Agg)> {
        self.policies
            .iter()
            .zip(&self.aggs)
            .filter(|(_, a)| a.in_context_ppb().is_none_or(|c| c <= max_in_context_ppb))
            .max_by_key(|(_, a)| a.f1_sum)
            .map(|(p, a)| (*p, *a))
    }
}
