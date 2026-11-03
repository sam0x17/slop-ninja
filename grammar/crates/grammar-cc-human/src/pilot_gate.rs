//! The pilot gate `r2_pilot_gate_v1` (protocol A2 §4.3, as amended by items
//! 1.17, 12.1–12.2 and 16.1; V22; build B12): a smoke check on one
//! D-item's keyed pilot before its bulk generation. Passing it is not
//! evidence of equivalence; a detected difference stops the bulk for a
//! pipeline fix.
//!
//! **Children** (the input contract). One JSONL row per pilot child: `id`,
//! `origin` (`mixed` or `model_only`), `parent_id`, `text`, a source
//! (`source`, or `source_kind` with `cohort_id`/`cohort` and `collection`,
//! read as `a_gates` reads rows; a child without one is refused), the spans
//! of a mixed child (`parent_spans`/`final_spans`, spliced `edits`, or local
//! `share_evidence`), and optionally `domain`, `source_group` and `block`.
//! A child's domain is its parent's bundle domain and its group its parent's
//! source group: when the parents file carries `domain` or `source_group`,
//! an absent child field takes the parent's and a different one is refused.
//! Only T-block children are taken (`block` absent or `T`); D4–D8 are
//! Train-side, and a C-block child is refused.
//!
//! **Pairs.** Each child is paired with its parent. A mirror (`model_only`)
//! pairs its whole text against the whole parent. A mixed child pairs the
//! lines of its generated spans against the lines of the parent spans they
//! replaced: the content lines whose body overlaps a span (for an
//! insertion, or a span between lines, the line holding its point),
//! measured with `layout_measure_v2` (amendments 7, 10 and 13; `counts_over`
//! on each contiguous run of those lines, summed; the record names the
//! measure).
//!
//! **Layout properties** (§4.3's audit list; a fixed set of six, item 12.2,
//! with item 16.1's boundary shares and wrap reading), per region. None
//! moves with a child's length when the child carries its parent's layout:
//! * `trailing_ws_line_share` and `leading_ws_line_share`: the shares of the
//!   region's lines with trailing and with leading whitespace;
//! * `single_newline_boundary_share`: the share of the region's paragraph
//!   boundaries that are one newline (sentence breaks and line breaks; a
//!   wrap is not a boundary);
//! * `blank_line_run_boundary_share`: the share of its boundaries that are
//!   blank-line runs;
//! * `hard_wrapped`: for a whole text, `measure`'s hard-wrapped flag (W).
//!   A mixed child's region reads 0 when its text is not hard-wrapped or one
//!   of its lines is unwrapped (`Measure::unwrapped`), 1 when it holds a
//!   wrap break, and otherwise nothing: lines no wider than W with no break
//!   between them fit both. A pair takes no difference when either side has
//!   no reading;
//! * for the separator-string distribution, `separator_parent_modal_share`:
//!   the share of the region's boundaries that take the parent region's most
//!   frequent separator (ties to the smallest escaped string), minus the
//!   parent region's share; 0 when the parent region has no boundary.
//!
//! A region with no boundary reads 0 for every boundary share, on either
//! side (for the separator, on the child's): a lost boundary counts against
//! the parent's kinds, and an added one, where the parent region has none,
//! for the child's. Raw break and run counts are not tested: a hard-wrapped
//! child's wraps grow with its words, and a line-form child's breaks with
//! its lines (item 16.1).
//!
//! **Test.** The unit is the source group (§11's pairing unit): a group's
//! difference is the mean, over its children in the domain, of child −
//! parent. Per domain × property, the exact two-sided sign test on the
//! groups' non-zero differences (|d| > 1e-12): p = min(1, 2·P(Bin(n, ½) ≥
//! max(n₊, n₋))). A cell is significant when p < 0.05 / K, with K = 6 × the
//! number of gated domains (Bonferroni).
//!
//! **Gated domains** (item 1.17): every domain × class cell the pilot
//! touches holds at least 59 children. Other domains are reported.
//!
//! **Adversary condition** (item 12.1). In every gated domain the
//! layout/typography adversary (`a_gates::adversary`, Brier units, this
//! run's configuration) is computed on two scopes with the same humans and
//! the same weighting:
//! * the pilot scope: the domain's humans (the humans file's T-block
//!   `human_only` rows of the domain) and the pilot's children there;
//! * the before scope: the same humans and R0's T-block positives of the
//!   domain (the before-rows file: R0's Train rows) whose (origin, source)
//!   pair is one the children there hold. It must hold every such pair, or
//!   the domain has no before figure.
//!
//! Both are weighted by `a_gates::fixed_base_rate_weights`: the humans take
//! 0.2 of the mass and the positives 0.8, split 55:25 when both positive
//! origins are present. The pilot passes the condition when its gain is
//! strictly below the before scope's.
//!
//! **Verdict.** `fail` when a gated cell is significant or a gated domain's
//! gain is not below its before figure; else `undetermined` when a gated
//! domain has no before figure or cannot be cross-fitted; else `pass`
//! (`vacuous` when no domain is gated). `paired_test_on` gives the paired
//! tests alone, reported without a verdict (G0a's Stage-1 layout-audit
//! report). Records hold counts, statistics and digests, never text.
use crate::a_gates::{self, ARow, AdvConfig, Block, ORIGINS};
use crate::measure::file_sha256;
use crate::store;
use anyhow::{Context, Result, bail, ensure};
use grammar_a_form::measure::{MEASURE_ID, Measure};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

pub const PILOT_SCHEMA: &str = "slop_ninja_a_r2_pilot_gate_v1";
pub const PAIRED_SCHEMA: &str = "slop_ninja_a_r2_paired_test_v1";
pub const PILOT_RULE: &str = "r2_pilot_gate_v1";
/// Two-sided level before the Bonferroni correction (§4.3).
pub const ALPHA: f64 = 0.05;
/// Group differences at or below this magnitude count as zero.
pub const ZERO_TOL: f64 = 1e-12;
pub const SEPARATOR_PROPERTY: &str = "separator_parent_modal_share";
/// The fixed property set (amendment 12 item 12.2).
pub const PROPERTIES: [&str; 6] =
    ["trailing_ws_line_share", "leading_ws_line_share", "single_newline_boundary_share", "blank_line_run_boundary_share", "hard_wrapped", SEPARATOR_PROPERTY];
pub const PROPERTIES_RULE: &str = "PROTOCOL-A2 amendment 12 item 12.2 as amended by amendment 16 item 16.1: line shares of trailing and leading whitespace; shares of the region's boundaries that are one newline and that are blank-line runs (a region with no boundary reads 0); hard_wrapped = the W flag for a whole text, and for a mixed child's region 0 when the text has no W or a region line is unwrapped, 1 when it holds a wrap break, else no reading (no difference); the parent's modal separator share (0 when the parent region has no boundary)";
pub const TEST_ID: &str = "exact two-sided sign test on source-group mean differences (child - parent), zeros dropped; Bonferroni over the 6 properties x gated domains; significant iff p < 0.05 / K";
pub const ADVERSARY_RULE: &str = "PROTOCOL-A2 amendment 12 item 12.1: per gated domain, the layout/typography adversary on (the domain's humans + the pilot's children) and on (the same humans + R0's positives of the domain with an (origin, source) pair the children hold, every pair present), both weighted humans 0.2, positives 0.8 (55:25 when both positive origins are present); pass iff the pilot gain < the before gain";

// ------------------------------------------------------------------ regions

/// Summed layout counts of a region.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Region {
    pub lines: usize,
    pub trailing_ws_lines: usize,
    pub leading_ws_lines: usize,
    pub single_breaks: usize,
    pub blank_line_runs: usize,
    pub wrap_breaks: usize,
    pub boundaries: usize,
    /// Boundaries that are one newline (sentence breaks and line breaks).
    pub single_newline_boundaries: usize,
    pub separators: BTreeMap<String, usize>,
    /// The `hard_wrapped` reading (module notes); None is no reading.
    pub hard_wrapped: Option<bool>,
}

fn share(a: usize, b: usize) -> f64 {
    if b == 0 { 0.0 } else { a as f64 / b as f64 }
}

impl Region {
    /// The value of one of the five region properties (the separator
    /// property is a pair's: see `difference`); None when the region has no
    /// reading (`hard_wrapped` only).
    pub fn value(&self, prop: &str) -> Option<f64> {
        match prop {
            "trailing_ws_line_share" => Some(share(self.trailing_ws_lines, self.lines)),
            "leading_ws_line_share" => Some(share(self.leading_ws_lines, self.lines)),
            "single_newline_boundary_share" => Some(share(self.single_newline_boundaries, self.boundaries)),
            "blank_line_run_boundary_share" => Some(share(self.blank_line_runs, self.boundaries)),
            "hard_wrapped" => self.hard_wrapped.map(|b| b as u8 as f64),
            p => panic!("unknown region property {p}"),
        }
    }

    /// The share of the region's boundaries taking the (escaped) separator `s`.
    pub fn separator_share(&self, s: &str) -> f64 {
        share(self.separators.get(s).copied().unwrap_or(0), self.boundaries)
    }

    /// The most frequent (escaped) separator; ties go to the smallest string.
    pub fn modal_separator(&self) -> Option<&str> {
        // BTreeMap order is ascending, so the first maximum is the smallest string.
        self.separators.iter().filter(|(_, n)| **n > 0).fold(None, |acc: Option<(&str, usize)>, (s, &n)| match acc {
            Some((_, m)) if m >= n => acc,
            _ => Some((s.as_str(), n)),
        }).map(|(s, _)| s)
    }
}

/// Child − parent difference of one property (module notes).
pub fn difference(child: &Region, parent: &Region, prop: &str) -> f64 {
    if prop == SEPARATOR_PROPERTY {
        match parent.modal_separator() {
            Some(m) => child.separator_share(m) - parent.separator_share(m),
            None => 0.0,
        }
    } else {
        match (child.value(prop), parent.value(prop)) {
            (Some(c), Some(p)) => c - p,
            _ => 0.0,
        }
    }
}

/// Content lines whose body overlaps a span; for a span that overlaps none
/// (an insertion, or a span inside a gap), the last line starting at or
/// before its point, else the first line. Sorted and unique.
pub fn span_lines(m: &Measure, spans: &[(usize, usize)]) -> Vec<usize> {
    let mut out = BTreeSet::new();
    for &(s, e) in spans {
        let hit: Vec<usize> = (0..m.lines.len()).filter(|&i| m.lines[i].body.0 < e.max(s + 1) && s < m.lines[i].body.1).collect();
        if hit.is_empty() {
            if let Some(i) = m.lines.iter().rposition(|l| l.body.0 <= s).or((!m.lines.is_empty()).then_some(0)) {
                out.insert(i);
            }
        } else {
            out.extend(hit);
        }
    }
    out.into_iter().collect()
}

/// The region of the given content lines: `counts_over` on each contiguous
/// run, summed (gaps between runs are outside the region). Its wrap reading
/// is a span region's (module notes).
pub fn region(m: &Measure, lines: &[usize]) -> Region {
    let mut r = Region::default();
    let mut a = 0;
    while a < lines.len() {
        let mut b = a + 1;
        while b < lines.len() && lines[b] == lines[b - 1] + 1 {
            b += 1;
        }
        let c = m.counts_over(lines[a]..lines[b - 1] + 1, false);
        r.lines += c.lines;
        r.trailing_ws_lines += c.trailing_ws_lines;
        r.leading_ws_lines += c.leading_ws_lines;
        r.single_breaks += c.single_breaks;
        r.blank_line_runs += c.blank_line_runs;
        r.wrap_breaks += c.wrap_breaks;
        r.boundaries += c.boundaries;
        r.single_newline_boundaries += c.sentence_breaks + c.line_breaks;
        for (k, v) in c.separators {
            *r.separators.entry(k).or_default() += v;
        }
        a = b;
    }
    r.hard_wrapped = if !m.hard_wrapped || lines.iter().any(|&i| m.unwrapped[i]) {
        Some(false)
    } else if r.wrap_breaks > 0 {
        Some(true)
    } else {
        None
    };
    r
}

/// The whole text as a region; its wrap reading is `measure`'s flag (W).
pub fn whole(text: &str) -> Region {
    let m = Measure::of(text);
    Region { hard_wrapped: Some(m.hard_wrapped), ..region(&m, &(0..m.lines.len()).collect::<Vec<_>>()) }
}

// ------------------------------------------------------------------ sign test

/// P(Bin(n, ½) ≥ k), summed in log space.
pub fn binom_half_upper_tail(n: usize, k: usize) -> f64 {
    if k == 0 {
        return 1.0;
    }
    if k > n {
        return 0.0;
    }
    // log C(n, j) for j = n down to k: C(n, j − 1) = C(n, j) · j / (n − j + 1).
    let mut lc = 0.0f64;
    let mut terms = vec![lc];
    for j in (k + 1..=n).rev() {
        lc += (j as f64).ln() - ((n - j + 1) as f64).ln();
        terms.push(lc);
    }
    let mx = terms.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let s: f64 = terms.iter().map(|t| (t - mx).exp()).sum();
    (mx + s.ln() - n as f64 * std::f64::consts::LN_2).exp().min(1.0)
}

/// The exact two-sided sign test: min(1, 2·P(Bin(n, ½) ≥ max(n₊, n₋))).
pub fn sign_test_p(pos: usize, neg: usize) -> f64 {
    let n = pos + neg;
    if n == 0 {
        return 1.0;
    }
    (2.0 * binom_half_upper_tail(n, pos.max(neg))).min(1.0)
}

/// The Bonferroni per-cell level and the significance rule.
pub fn bonferroni(k: usize) -> f64 {
    ALPHA / k.max(1) as f64
}

// ------------------------------------------------------------------ children

/// One pilot child, with its domain and group resolved against its parent.
#[derive(Clone, Debug)]
pub struct Child {
    pub id: String,
    pub domain: String,
    /// 1 (mixed) or 2 (model-only).
    pub origin: usize,
    pub parent_id: String,
    pub group: String,
    /// `a_gates::source_of`'s source (never `unspecified`).
    pub source: String,
    pub text: String,
    /// Mixed only: replaced parent spans and the child's generated spans.
    pub parent_spans: Spans,
    pub final_spans: Spans,
}

/// A parent row: its text and, when the parents file carries them, its
/// bundle domain and source group.
#[derive(Clone, Debug, Default)]
pub struct Parent {
    pub text: String,
    pub domain: Option<String>,
    pub group: Option<String>,
}

/// Byte spans `[start, end)`.
pub type Spans = Vec<(usize, usize)>;

fn pairs_of(v: &Value) -> Result<Spans> {
    v.as_array()
        .context("span list")?
        .iter()
        .map(|s| {
            let a = s.as_array().filter(|a| a.len() == 2).context("span [start, end]")?;
            let (x, y) = (a[0].as_u64().context("span start")? as usize, a[1].as_u64().context("span end")? as usize);
            ensure!(x <= y, "reversed span [{x}, {y}]");
            Ok((x, y))
        })
        .collect()
}

/// A mixed child's (parent spans, final spans): `parent_spans` and
/// `final_spans`; else `edits[].parent_span_bytes` / `final_span_bytes`
/// (spliced records); else `share_evidence.source_byte_spans` with
/// `share_evidence.final_byte_spans` or `ai_byte_spans` (local records).
pub fn spans_of(v: &Value) -> Result<(Spans, Spans)> {
    if v.get("parent_spans").is_some_and(|x| !x.is_null()) {
        return Ok((pairs_of(&v["parent_spans"])?, pairs_of(&v["final_spans"]).context("final_spans")?));
    }
    if let Some(edits) = v["edits"].as_array() {
        let mut p = Vec::new();
        let mut f = Vec::new();
        for e in edits {
            p.extend(pairs_of(&json!([e["parent_span_bytes"]]))?);
            f.extend(pairs_of(&json!([e["final_span_bytes"]]))?);
        }
        return Ok((p, f));
    }
    let se = &v["share_evidence"];
    if !se["source_byte_spans"].is_null() {
        let fin = if se["final_byte_spans"].is_null() { &v["ai_byte_spans"] } else { &se["final_byte_spans"] };
        return Ok((pairs_of(&se["source_byte_spans"])?, pairs_of(fin).context("final spans")?));
    }
    bail!("a mixed child needs parent_spans and final_spans, edits, or share_evidence spans")
}

fn check_spans(text: &str, spans: &[(usize, usize)], what: &str) -> Result<()> {
    for &(s, e) in spans {
        ensure!(e <= text.len() && text.is_char_boundary(s) && text.is_char_boundary(e), "{what} span [{s}, {e}] is not a character range of the text");
    }
    Ok(())
}

/// A child field resolved against the parent's: an absent child field takes
/// the parent's, and a different one is refused.
fn resolve(id: &str, what: &str, child: Option<&str>, parent: Option<&str>) -> Result<String> {
    match (child, parent) {
        (Some(c), Some(p)) => {
            ensure!(c == p, "child {id}: {what} {c:?} is not its parent's {p:?}");
            Ok(c.to_string())
        }
        (Some(c), None) => Ok(c.to_string()),
        (None, Some(p)) => Ok(p.to_string()),
        (None, None) => bail!("child {id}: no {what}, and its parent row carries none"),
    }
}

/// Parses one child against its parent (module notes: the input contract).
pub fn parse_child(v: &Value, parents: &HashMap<String, Parent>) -> Result<Child> {
    let s = |k: &str| v[k].as_str().map(String::from).with_context(|| format!("field {k}"));
    let id = s("id")?;
    let origin = a_gates::origin_index(&s("origin")?)?;
    ensure!(origin != 0, "child {id}: a pilot child is mixed or model_only");
    let parent_id = s("parent_id").with_context(|| format!("child {id}"))?;
    let text = s("text").with_context(|| format!("child {id}"))?;
    match v["block"].as_str() {
        None => {}
        Some(b) => ensure!(Block::parse(b)? == Block::T, "child {id}: a C-block child is refused (the pilot gate takes Train-side T-domain pilots, D4-D8)"),
    }
    let parent = parents.get(&parent_id).with_context(|| format!("child {id}: parent {parent_id} not found"))?;
    let domain = resolve(&id, "domain", v["domain"].as_str(), parent.domain.as_deref())?;
    let group = resolve(&id, "source_group", v["source_group"].as_str(), parent.group.as_deref())?;
    let cohort = v["cohort_id"].as_str().or(v["cohort"].as_str());
    let source = a_gates::source_of(origin, v["source_kind"].as_str(), cohort, v["collection"].as_str(), Block::T, v["source"].as_str()).with_context(|| format!("child {id}"))?;
    ensure!(source != "unspecified", "child {id}: no source (give source, or source_kind)");
    let (parent_spans, final_spans) = if origin == 1 { spans_of(v).with_context(|| format!("child {id}"))? } else { (Vec::new(), Vec::new()) };
    if origin == 1 {
        ensure!(!parent_spans.is_empty() && !final_spans.is_empty(), "child {id}: a mixed child needs its spans");
        check_spans(&text, &final_spans, "final").with_context(|| format!("child {id}"))?;
    }
    Ok(Child { id, domain, origin, parent_id, group, source, text, parent_spans, final_spans })
}

/// The child's and the parent's regions.
pub fn pair_regions(c: &Child, parent: &str) -> Result<(Region, Region)> {
    if c.origin == 2 {
        return Ok((whole(&c.text), whole(parent)));
    }
    check_spans(parent, &c.parent_spans, "parent").with_context(|| format!("child {}", c.id))?;
    let mc = Measure::of(&c.text);
    let mp = Measure::of(parent);
    Ok((region(&mc, &span_lines(&mc, &c.final_spans)), region(&mp, &span_lines(&mp, &c.parent_spans))))
}

// ------------------------------------------------------------------ paired tests

/// Per-domain paired tests: (domain → property → group mean differences).
pub type GroupDiffs = BTreeMap<String, BTreeMap<String, Vec<f64>>>;
/// Per (domain, group): summed differences per property and the child count.
type GroupSums<'a> = BTreeMap<(&'a str, &'a str), ([f64; 6], usize)>;

/// Group mean differences per domain over the fixed properties (module notes).
pub fn group_differences(children: &[Child], regions: &[(Region, Region)]) -> GroupDiffs {
    let mut sums: GroupSums = BTreeMap::new();
    for (c, (rc, rp)) in children.iter().zip(regions) {
        let e = sums.entry((c.domain.as_str(), c.group.as_str())).or_default();
        e.1 += 1;
        for (k, p) in PROPERTIES.iter().enumerate() {
            e.0[k] += difference(rc, rp, p);
        }
    }
    let mut out: GroupDiffs = BTreeMap::new();
    for ((d, _), (m, k)) in sums {
        let dm = out.entry(d.to_string()).or_default();
        for (p, s) in PROPERTIES.iter().zip(m) {
            dm.entry(p.to_string()).or_default().push(s / k as f64);
        }
    }
    out
}

/// One cell's test record.
pub fn cell_test(diffs: &[f64], alpha_cell: f64) -> Value {
    let pos = diffs.iter().filter(|&&d| d > ZERO_TOL).count();
    let neg = diffs.iter().filter(|&&d| d < -ZERO_TOL).count();
    let p = sign_test_p(pos, neg);
    json!({"groups": diffs.len(), "groups_nonzero": pos + neg, "positive": pos, "negative": neg,
        "mean_difference": diffs.iter().sum::<f64>() / diffs.len().max(1) as f64, "p_value": p, "significant": p < alpha_cell})
}

/// The paired tests of a set of children.
pub struct PairedTests {
    /// Domain → class → children.
    pub cells: BTreeMap<String, BTreeMap<&'static str, usize>>,
    pub gated: BTreeMap<String, bool>,
    /// Domain → property → test record.
    pub tests: BTreeMap<String, BTreeMap<String, Value>>,
    pub groups: BTreeMap<String, usize>,
    pub k: usize,
    pub alpha_cell: f64,
    /// Significant cells of gated domains.
    pub significant: Vec<String>,
}

/// Regions, group differences and sign tests of the children against their
/// parents' texts.
pub fn paired_tests(children: &[Child], parents: &HashMap<String, String>) -> Result<PairedTests> {
    ensure!(!children.is_empty(), "no pilot children");
    let mut regions = Vec::with_capacity(children.len());
    for c in children {
        let p = parents.get(&c.parent_id).with_context(|| format!("child {}: parent {} not found", c.id, c.parent_id))?;
        regions.push(pair_regions(c, p)?);
    }
    let mut cells: BTreeMap<String, BTreeMap<&'static str, usize>> = BTreeMap::new();
    for c in children {
        *cells.entry(c.domain.clone()).or_default().entry(ORIGINS[c.origin]).or_default() += 1;
    }
    let gated: BTreeMap<String, bool> = cells.iter().map(|(d, m)| (d.clone(), m.values().all(|&n| n >= a_gates::COVERAGE_FLOOR))).collect();
    let diffs = group_differences(children, &regions);
    let k: usize = diffs.iter().filter(|(d, _)| gated[d.as_str()]).map(|(_, m)| m.len()).sum();
    let alpha_cell = bonferroni(k);
    let mut tests = BTreeMap::new();
    let mut groups = BTreeMap::new();
    let mut significant = Vec::new();
    for (d, props) in &diffs {
        let t: BTreeMap<String, Value> = props.iter().map(|(p, v)| (p.clone(), cell_test(v, alpha_cell))).collect();
        if gated[d.as_str()] {
            for (p, x) in &t {
                if x["significant"] == true {
                    significant.push(format!("{d}: {p} differs (p = {})", x["p_value"]));
                }
            }
        }
        groups.insert(d.clone(), props.values().next().map_or(0, |v| v.len()));
        tests.insert(d.clone(), t);
    }
    Ok(PairedTests { cells, gated, tests, groups, k, alpha_cell, significant })
}

fn paired_header(t: &PairedTests, children: usize) -> Value {
    json!({"rule": PILOT_RULE, "test": TEST_ID, "measure": MEASURE_ID, "alpha": ALPHA, "bonferroni_cells": t.k, "alpha_per_cell": t.alpha_cell,
        "coverage_floor": a_gates::COVERAGE_FLOOR, "properties": PROPERTIES, "properties_rule": PROPERTIES_RULE, "children": children})
}

/// The paired tests alone, reported without a verdict (G0a's Stage-1
/// layout-audit report, §11.1).
pub fn paired_test_on(children: &[Child], parents: &HashMap<String, String>) -> Result<Value> {
    let t = paired_tests(children, parents)?;
    let mut out = paired_header(&t, children.len());
    out["schema"] = json!(PAIRED_SCHEMA);
    out["mode"] = json!("paired_only");
    out["domains"] = json!(t.tests.iter().map(|(d, x)| (d.clone(), json!({"gated": t.gated[d], "cells": t.cells[d], "groups": t.groups[d], "properties": x}))).collect::<BTreeMap<_, _>>());
    out["report"] = json!({"status": "reported", "gated_domains": t.gated.values().filter(|&&g| g).count(), "significant_gated_cells": t.significant,
        "note": "the paired child - parent test reported with the layout audit; no verdict"});
    Ok(out)
}

// ------------------------------------------------------------------ adversary condition

/// The adversary condition of one domain.
pub enum DomainAdversary {
    Below,
    NotBelow,
    NoBefore(String),
    NotComputable(String),
}

fn counts_by<'a>(it: impl Iterator<Item = (usize, &'a str)>) -> BTreeMap<String, usize> {
    let mut m = BTreeMap::new();
    for (o, s) in it {
        *m.entry(format!("{}|{s}", ORIGINS[o])).or_insert(0usize) += 1;
    }
    m
}

/// The adversary on (humans + positives), weighted by `fixed_base_rate_weights`.
/// A scope's adversary result, its Kish ESS and origin shares, or why it cannot be cross-fitted.
type ScopeGain = std::result::Result<(a_gates::AdvResult, f64, [f64; 3]), String>;

fn scope_gain(hfeats: &[Vec<f64>], hgroups: &[&str], pfeats: Vec<Vec<f64>>, porigins: &[usize], pgroups: &[&str], cfg: &AdvConfig) -> Result<ScopeGain> {
    let mut feats = hfeats.to_vec();
    feats.extend(pfeats);
    let origins: Vec<usize> = std::iter::repeat_n(0, hfeats.len()).chain(porigins.iter().copied()).collect();
    let groups: Vec<&str> = hgroups.iter().chain(pgroups).copied().collect();
    let w = a_gates::fixed_base_rate_weights(&origins);
    let y: Vec<f64> = origins.iter().map(|&o| (o != 0) as u8 as f64).collect();
    Ok(a_gates::adversary(&feats, &y, &w, &groups, &origins, cfg)?.map(|r| (r, a_gates::kish_ess(&w), a_gates::fixed_base_rate_shares(&origins))))
}

/// Item 12.1's condition in domain `d`: the pilot scope against the before
/// scope (module notes). `humans` and `before` may hold other domains.
pub fn domain_adversary(d: &str, children: &[Child], humans: &[ARow], before: &[ARow], cfg: &AdvConfig) -> Result<(DomainAdversary, Value)> {
    let hum: Vec<&ARow> = humans.iter().filter(|h| h.origin == 0 && h.block == Block::T && h.domain == d).collect();
    let kids: Vec<&Child> = children.iter().filter(|c| c.domain == d).collect();
    let pairs: BTreeSet<(usize, &str)> = kids.iter().map(|c| (c.origin, c.source.as_str())).collect();
    let pos: Vec<&ARow> = before.iter().filter(|r| r.origin != 0 && r.block == Block::T && r.domain == d && pairs.contains(&(r.origin, r.source.as_str()))).collect();
    let present: BTreeSet<(usize, &str)> = pos.iter().map(|r| (r.origin, r.source.as_str())).collect();
    let mut rec = json!({"rule": ADVERSARY_RULE, "humans": hum.len(), "children": kids.len(), "before_positives": pos.len(),
        "children_by_origin_source": counts_by(kids.iter().map(|c| (c.origin, c.source.as_str()))),
        "before_by_origin_source": counts_by(pos.iter().map(|r| (r.origin, r.source.as_str())))});
    let missing: Vec<String> = pairs.difference(&present).map(|(o, s)| format!("{}|{s}", ORIGINS[*o])).collect();
    if hum.is_empty() {
        rec["status"] = json!("not_computable");
        rec["reason"] = json!("no humans of the domain");
        return Ok((DomainAdversary::NotComputable("no humans of the domain".into()), rec));
    }
    let groups_of = |r: &ARow| -> Result<String> { r.group.clone().with_context(|| format!("row {} has no source_group (folds are by source group)", r.id)) };
    let hgroups_owned: Vec<String> = hum.iter().map(|&h| groups_of(h)).collect::<Result<_>>()?;
    let hgroups: Vec<&str> = hgroups_owned.iter().map(String::as_str).collect();
    let hfeats: Vec<Vec<f64>> = hum.iter().map(|h| a_gates::features(&h.text, None).layout).collect();
    let pilot = scope_gain(&hfeats, &hgroups, kids.iter().map(|c| a_gates::features(&c.text, None).layout).collect(), &kids.iter().map(|c| c.origin).collect::<Vec<_>>(), &kids.iter().map(|c| c.group.as_str()).collect::<Vec<_>>(), cfg)?;
    let pilot = match pilot {
        Ok(p) => p,
        Err(why) => {
            rec["status"] = json!("not_computable");
            rec["reason"] = json!(format!("pilot scope: {why}"));
            return Ok((DomainAdversary::NotComputable(format!("pilot scope: {why}")), rec));
        }
    };
    let prior = |r: &a_gates::AdvResult| r.repeats.iter().filter_map(|x| x["prior_brier"].as_f64()).sum::<f64>() / r.repeats.len().max(1) as f64;
    rec["pilot_gain_brier"] = json!(pilot.0.gain);
    rec["pilot_prior_brier"] = json!(prior(&pilot.0));
    rec["pilot_upper95_brier"] = json!(pilot.0.boot95.1);
    rec["origin_shares"] = json!(pilot.2);
    rec["kish_ess"] = json!({"pilot": pilot.1});
    if !missing.is_empty() {
        let why = format!("R0 holds no positive of {} in this domain", missing.join(", "));
        rec["status"] = json!("no_before_figure");
        rec["reason"] = json!(why);
        return Ok((DomainAdversary::NoBefore(why), rec));
    }
    let pgroups_owned: Vec<String> = pos.iter().map(|&r| groups_of(r)).collect::<Result<_>>()?;
    let bfore = scope_gain(&hfeats, &hgroups, pos.iter().map(|r| a_gates::features(&r.text, None).layout).collect(), &pos.iter().map(|r| r.origin).collect::<Vec<_>>(), &pgroups_owned.iter().map(String::as_str).collect::<Vec<_>>(), cfg)?;
    let bfore = match bfore {
        Ok(b) => b,
        Err(why) => {
            rec["status"] = json!("not_computable");
            rec["reason"] = json!(format!("before scope: {why}"));
            return Ok((DomainAdversary::NotComputable(format!("before scope: {why}")), rec));
        }
    };
    ensure!(bfore.2 == pilot.2, "domain {d}: the two scopes' origin shares differ");
    let below = pilot.0.gain < bfore.0.gain;
    rec["before_gain_brier"] = json!(bfore.0.gain);
    rec["before_prior_brier"] = json!(prior(&bfore.0));
    rec["before_upper95_brier"] = json!(bfore.0.boot95.1);
    rec["kish_ess"]["before"] = json!(bfore.1);
    rec["below_before"] = json!(below);
    rec["status"] = json!(if below { "below_before" } else { "not_below_before" });
    Ok((if below { DomainAdversary::Below } else { DomainAdversary::NotBelow }, rec))
}

// ------------------------------------------------------------------ the gate

/// The pilot gate on prepared inputs (pure apart from the adversary's work):
/// resolved children, their parents' texts, the humans of the children's
/// domains and R0's Train rows for the before scopes.
pub fn pilot_gate_on(children: &[Child], parents: &HashMap<String, String>, humans: &[ARow], before: &[ARow], cfg: &AdvConfig) -> Result<Value> {
    let t = paired_tests(children, parents)?;
    let mut failures = t.significant.clone();
    let mut undetermined = Vec::new();
    let mut domains = BTreeMap::new();
    for (d, props) in &t.tests {
        let g = t.gated[d];
        let (outcome, adv) = domain_adversary(d, children, humans, before, cfg)?;
        if g {
            match outcome {
                DomainAdversary::Below => {}
                DomainAdversary::NotBelow => failures.push(format!("{d}: layout adversary gain {} is not below the before figure {}", adv["pilot_gain_brier"], adv["before_gain_brier"])),
                DomainAdversary::NoBefore(why) => undetermined.push(format!("{d}: no before figure ({why})")),
                DomainAdversary::NotComputable(why) => undetermined.push(format!("{d}: adversary not computable ({why})")),
            }
        }
        domains.insert(d.clone(), json!({"gated": g, "cells": t.cells[d], "groups": t.groups[d], "properties": props, "adversary": adv}));
    }
    let n_gated = t.gated.values().filter(|&&g| g).count();
    let status = if !failures.is_empty() { "fail" } else if !undetermined.is_empty() { "undetermined" } else { "pass" };
    let mut out = paired_header(&t, children.len());
    out["schema"] = json!(PILOT_SCHEMA);
    out["adversary_rule"] = json!(ADVERSARY_RULE);
    out["domains"] = json!(domains);
    out["verdict"] = json!({"status": status, "gated_domains": n_gated, "vacuous": n_gated == 0, "failures": failures, "undetermined": undetermined,
        "note": "smoke check (item 1.17): a detected difference stops the bulk; passing is not evidence of equivalence"});
    Ok(out)
}

pub struct PilotArgs {
    pub pilot: PathBuf,
    pub parents: PathBuf,
    pub humans: PathBuf,
    /// R0's Train rows holding the before scopes' positives.
    pub before_rows: PathBuf,
    pub cfg: AdvConfig,
    pub out: PathBuf,
}

pub struct PairedArgs {
    pub pilot: PathBuf,
    pub parents: PathBuf,
    pub out: PathBuf,
}

fn read_parents(path: &Path, want: &HashSet<String>) -> Result<HashMap<String, Parent>> {
    let r = BufReader::new(std::fs::File::open(path).with_context(|| format!("open {}", path.display()))?);
    let mut m = HashMap::new();
    for (i, l) in r.lines().enumerate() {
        let l = l?;
        if l.trim().is_empty() {
            continue;
        }
        let v: Value = serde_json::from_str(&l).with_context(|| format!("{} line {}", path.display(), i + 1))?;
        let id = v["id"].as_str().with_context(|| format!("{} line {}: id", path.display(), i + 1))?;
        if want.contains(id) {
            let text = v["text"].as_str().with_context(|| format!("{}: parent {id} has no text", path.display()))?.to_string();
            let p = Parent { text, domain: v["domain"].as_str().map(String::from), group: v["source_group"].as_str().map(String::from) };
            ensure!(m.insert(id.to_string(), p).is_none(), "{}: duplicate parent {id}", path.display());
        }
    }
    Ok(m)
}

/// Reads the children and resolves them against their parents.
fn read_children(pilot: &Path, parents_path: &Path) -> Result<(Vec<Child>, HashMap<String, String>)> {
    let r = BufReader::new(std::fs::File::open(pilot).with_context(|| format!("open {}", pilot.display()))?);
    let mut raw = Vec::new();
    for (i, l) in r.lines().enumerate() {
        let l = l?;
        if l.trim().is_empty() {
            continue;
        }
        raw.push((i + 1, serde_json::from_str::<Value>(&l).with_context(|| format!("{} line {}", pilot.display(), i + 1))?));
    }
    let want: HashSet<String> = raw.iter().filter_map(|(_, v)| v["parent_id"].as_str().map(String::from)).collect();
    let parents = read_parents(parents_path, &want)?;
    let mut children = Vec::with_capacity(raw.len());
    let mut seen = HashSet::new();
    for (line, v) in &raw {
        let c = parse_child(v, &parents).with_context(|| format!("{} line {line}", pilot.display()))?;
        ensure!(seen.insert(c.id.clone()), "duplicate child {}", c.id);
        children.push(c);
    }
    let texts = parents.into_iter().map(|(k, p)| (k, p.text)).collect();
    Ok((children, texts))
}

/// The `pilot-gate` command: writes `pilot-gate.json` under `out`.
pub fn pilot_gate(a: &PilotArgs) -> Result<Value> {
    let (children, parents) = read_children(&a.pilot, &a.parents)?;
    let domains: HashSet<&str> = children.iter().map(|c| c.domain.as_str()).collect();
    let pairs: HashSet<(&str, usize, &str)> = children.iter().map(|c| (c.domain.as_str(), c.origin, c.source.as_str())).collect();
    let (humans, _) = a_gates::read_rows_filtered(&a.humans, true, |r| r.origin == 0 && r.block == Block::T && domains.contains(r.domain.as_str()))?;
    let (before, _) = a_gates::read_rows_filtered(&a.before_rows, true, |r| r.origin != 0 && r.block == Block::T && pairs.contains(&(r.domain.as_str(), r.origin, r.source.as_str())))?;
    let mut out = pilot_gate_on(&children, &parents, &humans, &before, &a.cfg)?;
    out["inputs"] = json!({"pilot": a.pilot, "pilot_sha256": file_sha256(&a.pilot)?, "parents": a.parents, "parents_sha256": file_sha256(&a.parents)?,
        "humans": a.humans, "humans_sha256": file_sha256(&a.humans)?, "humans_rows": humans.len(),
        "before_rows": a.before_rows, "before_rows_sha256": file_sha256(&a.before_rows)?, "before_rows_matched": before.len(), "config": a.cfg.record()});
    store::write_json(&a.out.join("pilot-gate.json"), &out)?;
    Ok(json!({"status": out["verdict"]["status"], "gated_domains": out["verdict"]["gated_domains"], "children": children.len()}))
}

/// The `paired-test` command: writes `paired-test.json` under `out`.
pub fn paired_test(a: &PairedArgs) -> Result<Value> {
    let (children, parents) = read_children(&a.pilot, &a.parents)?;
    let mut out = paired_test_on(&children, &parents)?;
    out["inputs"] = json!({"pilot": a.pilot, "pilot_sha256": file_sha256(&a.pilot)?, "parents": a.parents, "parents_sha256": file_sha256(&a.parents)?});
    store::write_json(&a.out.join("paired-test.json"), &out)?;
    Ok(json!({"status": "reported", "gated_domains": out["report"]["gated_domains"], "children": children.len()}))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sign_test_p_values_are_exact() {
        assert_eq!(sign_test_p(0, 0), 1.0);
        assert!((sign_test_p(10, 0) - 2.0 / 1024.0).abs() < 1e-15);
        assert!((sign_test_p(0, 10) - 2.0 / 1024.0).abs() < 1e-15);
        // n = 5, k = 4: P(X >= 4) = (5 + 1)/32; two-sided 12/32.
        assert!((sign_test_p(4, 1) - 0.375).abs() < 1e-15);
        assert_eq!(sign_test_p(2, 2), 1.0);
        assert!((binom_half_upper_tail(4, 2) - 11.0 / 16.0).abs() < 1e-15);
        assert_eq!(binom_half_upper_tail(3, 4), 0.0);
        // Large n stays finite: n = 2000, k = 1100 (z ≈ 4.47).
        let p = sign_test_p(1100, 900);
        assert!(p > 0.0 && p < 1e-5, "{p}");
    }

    #[test]
    fn bonferroni_divides_the_level_by_the_cell_count() {
        assert_eq!(bonferroni(1), 0.05);
        assert!((bonferroni(12) - 0.05 / 12.0).abs() < 1e-18);
        assert_eq!(bonferroni(0), 0.05);
        // 10 of 10 groups one way: p = 2/1024 ≈ 0.00195 is significant at K = 12
        // (0.00417) and not at K = 30 (0.00167).
        let d = vec![1.0; 10];
        assert_eq!(cell_test(&d, bonferroni(12))["significant"], true);
        assert_eq!(cell_test(&d, bonferroni(30))["significant"], false);
        // Zeros are dropped.
        let z = cell_test(&[0.0, 0.0, 1e-13, 1.0], bonferroni(1));
        assert_eq!((z["groups_nonzero"].as_u64(), z["p_value"].as_f64()), (Some(1), Some(1.0)));
    }

    #[test]
    fn regions_follow_whole_texts_and_replaced_lines() {
        let parent = "First line of the letter here. \nSecond line follows it now. \nThird line ends the note.";
        let m = Measure::of(parent);
        // A span inside line 2 touches line 2 only.
        let s = parent.find("Second").unwrap();
        assert_eq!(span_lines(&m, &[(s, s + 6)]), vec![1]);
        // A span from line 1 into line 3 touches all three.
        assert_eq!(span_lines(&m, &[(5, parent.find("ends").unwrap())]), vec![0, 1, 2]);
        // An insertion at a line start holds that line; inside a gap, the line before.
        assert_eq!(span_lines(&m, &[(s, s)]), vec![1]);
        let gap = parent.find('\n').unwrap();
        assert_eq!(span_lines(&m, &[(gap, gap)]), vec![0]);
        let r = region(&m, &[0, 2]);
        assert_eq!((r.lines, r.trailing_ws_lines, r.single_breaks, r.boundaries), (2, 1, 0, 0));
        let w = whole(parent);
        assert_eq!((w.lines, w.trailing_ws_lines, w.single_breaks, w.boundaries), (3, 2, 2, 2));
        assert!((w.value("trailing_ws_line_share").unwrap() - 2.0 / 3.0).abs() < 1e-12);
        assert_eq!(w.separator_share("\\n"), 1.0);
        assert_eq!(w.value("hard_wrapped"), Some(0.0));
        let wrapped = whole("The quick brown fox jumps over\nthe lazy dog. It ran.\nThen it slept under the old\noak tree for an hour.");
        assert_eq!(wrapped.value("hard_wrapped"), Some(1.0));
        assert_eq!(Region::default().value("leading_ws_line_share"), Some(0.0));
    }

    #[test]
    fn the_separator_property_is_signed_against_the_parents_modal_separator() {
        let r = |seps: &[(&str, usize)]| Region { boundaries: seps.iter().map(|x| x.1).sum(), separators: seps.iter().map(|(s, n)| (s.to_string(), *n)).collect(), ..Region::default() };
        // Ties go to the smallest escaped string: "\\n" < "\\n\\n".
        assert_eq!(r(&[("\\n\\n", 2), ("\\n", 2)]).modal_separator(), Some("\\n"));
        assert_eq!(r(&[("\\n\\n", 3), ("\\n", 2)]).modal_separator(), Some("\\n\\n"));
        assert_eq!(Region::default().modal_separator(), None);
        let parent = r(&[("\\n", 3), ("\\n\\n", 1)]);
        // A child normalised to blank lines loses the parent's mode: 0 - 0.75.
        assert!((difference(&r(&[("\\n\\n", 4)]), &parent, SEPARATOR_PROPERTY) + 0.75).abs() < 1e-15);
        // A child collapsed onto the mode gains: 1 - 0.75.
        assert!((difference(&r(&[("\\n", 2)]), &parent, SEPARATOR_PROPERTY) - 0.25).abs() < 1e-15);
        // A faithful child: 0. A parent region with no boundary: 0.
        assert_eq!(difference(&r(&[("\\n", 6), ("\\n\\n", 2)]), &parent, SEPARATOR_PROPERTY), 0.0);
        assert_eq!(difference(&r(&[("\\n", 2)]), &Region::default(), SEPARATOR_PROPERTY), 0.0);
    }

    fn parents_of(m: &[(&str, &str, Option<&str>, Option<&str>)]) -> HashMap<String, Parent> {
        m.iter().map(|(id, t, d, g)| (id.to_string(), Parent { text: t.to_string(), domain: d.map(String::from), group: g.map(String::from) })).collect()
    }

    #[test]
    fn spans_come_from_every_record_shape() {
        let a = json!({"parent_spans": [[1, 4]], "final_spans": [[1, 6]]});
        assert_eq!(spans_of(&a).unwrap(), (vec![(1, 4)], vec![(1, 6)]));
        let b = json!({"edits": [{"parent_span_bytes": [2, 5], "final_span_bytes": [2, 9]}, {"parent_span_bytes": [10, 10], "final_span_bytes": [14, 20]}]});
        assert_eq!(spans_of(&b).unwrap(), (vec![(2, 5), (10, 10)], vec![(2, 9), (14, 20)]));
        let c = json!({"share_evidence": {"source_byte_spans": [[0, 3]]}, "ai_byte_spans": [[0, 5]]});
        assert_eq!(spans_of(&c).unwrap(), (vec![(0, 3)], vec![(0, 5)]));
        let c2 = json!({"share_evidence": {"source_byte_spans": [[0, 3]], "final_byte_spans": [[0, 4]]}, "ai_byte_spans": [[0, 5]]});
        assert_eq!(spans_of(&c2).unwrap().1, vec![(0, 4)]);
        assert!(spans_of(&json!({"ai_byte_spans": [[0, 5]]})).is_err());
        assert!(spans_of(&json!({"parent_spans": [[4, 1]], "final_spans": [[1, 2]]})).is_err());
        let ps = parents_of(&[("p", "abc", None, Some("g"))]);
        // A mixed child without spans, and a span off a character boundary, are refused.
        assert!(parse_child(&json!({"id": "x", "origin": "mixed", "domain": "d", "parent_id": "p", "text": "abc", "source": "local"}), &ps).is_err());
        assert!(parse_child(&json!({"id": "x", "origin": "mixed", "domain": "d", "parent_id": "p", "text": "a\u{e9}c", "source": "local", "parent_spans": [[0, 1]], "final_spans": [[0, 2]]}), &ps).is_err());
        assert!(parse_child(&json!({"id": "x", "origin": "human_only", "domain": "d", "parent_id": "p", "text": "abc", "source": "local"}), &ps).is_err());
        let m = parse_child(&json!({"id": "x", "origin": "model_only", "domain": "d", "parent_id": "p", "text": "abc", "source_kind": "local_positive"}), &ps).unwrap();
        assert_eq!((m.group.as_str(), m.source.as_str()), ("g", "local"));
    }

    #[test]
    fn children_follow_the_input_contract() {
        let ps = parents_of(&[("p", "abc", Some("news"), Some("g7")), ("q", "abc", None, None)]);
        let child = |extra: Value| {
            let mut v = json!({"id": "x", "origin": "model_only", "parent_id": "p", "text": "abc", "source_kind": "local_positive"});
            for (k, x) in extra.as_object().unwrap() {
                v[k] = x.clone();
            }
            parse_child(&v, &ps)
        };
        // Domain and group come from the parent row when absent, and must equal it when given.
        let c = child(json!({})).unwrap();
        assert_eq!((c.domain.as_str(), c.group.as_str()), ("news", "g7"));
        assert!(child(json!({"domain": "news", "source_group": "g7", "block": "T"})).is_ok());
        assert!(child(json!({"domain": "reviews"})).unwrap_err().to_string().contains("domain"));
        assert!(child(json!({"source_group": "p"})).unwrap_err().to_string().contains("source_group"));
        // A parent row without them needs the child's own.
        assert!(child(json!({"parent_id": "q"})).is_err());
        assert!(child(json!({"parent_id": "q", "domain": "news", "source_group": "g7"})).is_ok());
        // C-block children, children without a source and unknown parents are refused.
        assert!(child(json!({"block": "C"})).unwrap_err().to_string().contains("C-block"));
        assert!(child(json!({"source_kind": null})).unwrap_err().to_string().contains("source"));
        assert!(child(json!({"parent_id": "zz"})).is_err());
        assert_eq!(child(json!({"source_kind": null, "source": "spliced"})).unwrap().source, "spliced");
    }

    fn words(g: usize, k: usize) -> String {
        (0..9).map(|j| format!("w{}", (g * 3 + j * k) % 17)).collect::<Vec<_>>().join(" ")
    }

    fn human(id: &str, domain: &str, group: &str, text: String) -> ARow {
        ARow { id: id.into(), text, origin: 0, domain: domain.into(), group: Some(group.into()), block: Block::T, weight: 1.0, source: "pool".into(), class: "human_pool".into() }
    }

    fn positive(id: &str, origin: usize, source: &str, domain: &str, group: &str, text: String) -> ARow {
        ARow { id: id.into(), text, origin, domain: domain.into(), group: Some(group.into()), block: Block::T, weight: 1.0, source: source.into(), class: ORIGINS[origin].into() }
    }

    fn mirror(id: &str, parent: &str, group: &str, text: String) -> Child {
        Child { id: id.into(), domain: "news".into(), origin: 2, parent_id: parent.into(), group: group.into(), source: "local".into(), text, parent_spans: vec![], final_spans: vec![] }
    }

    /// v1's excerpt layout: blank-line paragraphs, no trailing whitespace.
    fn excerpt(g: usize) -> String {
        format!("{}.\n\n{}.\n\n{}.", words(g, 4), words(g, 5), words(g, 6))
    }

    /// Parents with trailing whitespace on single-newline lines (the domain's
    /// humans); their mirrors either copy the layout or drop it.
    fn fixture(n_groups: usize, faithful: bool) -> (Vec<Child>, HashMap<String, String>, Vec<ARow>) {
        let mut children = Vec::new();
        let mut parents = HashMap::new();
        let mut humans = Vec::new();
        for g in 0..n_groups {
            let parent = format!("{}. \n{}. \n{}.", words(g, 1), words(g, 2), words(g, 3));
            let pid = format!("p{g}");
            parents.insert(pid.clone(), parent.clone());
            humans.push(human(&pid, "news", &format!("g{g}"), parent));
            let text = if faithful { format!("{}. \n{}. \n{}.", words(g, 4), words(g, 5), words(g, 6)) } else { excerpt(g) };
            children.push(mirror(&format!("c{g}"), &pid, &format!("g{g}"), text));
        }
        (children, parents, humans)
    }

    /// R0's mirrors of the same parents in v1's excerpt layout.
    fn r0_excerpt_mirrors(n_groups: usize) -> Vec<ARow> {
        (0..n_groups).map(|g| positive(&format!("r0m{g}"), 2, "local", "news", &format!("g{g}"), excerpt(g))).collect()
    }

    fn cfg() -> AdvConfig {
        AdvConfig { draws: 20, cv_repeats: 1, ..AdvConfig::default() }
    }

    #[test]
    fn a_pilot_that_drops_the_parents_layout_fails() {
        let (children, parents, humans) = fixture(60, false);
        let v = pilot_gate_on(&children, &parents, &humans, &r0_excerpt_mirrors(60), &cfg()).unwrap();
        assert_eq!(v["verdict"]["status"], "fail");
        let d = &v["domains"]["news"];
        assert_eq!(d["gated"], true);
        assert_eq!(d["properties"]["trailing_ws_line_share"]["negative"], 60);
        assert_eq!(d["properties"]["trailing_ws_line_share"]["significant"], true);
        // Every single-newline boundary became a blank-line run.
        assert_eq!(d["properties"]["single_newline_boundary_share"]["negative"], 60);
        assert_eq!(d["properties"]["single_newline_boundary_share"]["significant"], true);
        assert_eq!(d["properties"]["blank_line_run_boundary_share"]["positive"], 60);
        // The parent's mode "\n" is lost in every pair.
        assert_eq!(d["properties"][SEPARATOR_PROPERTY]["negative"], 60);
        // The property set is fixed: six cells per gated domain, whatever separators appear.
        assert_eq!(v["bonferroni_cells"], 6);
        assert_eq!(d["properties"].as_object().unwrap().len(), 6);
        let raw = v.to_string();
        assert!(!raw.contains("w1 w") && !raw.contains("w4 "), "no text in the record");
    }

    #[test]
    fn a_faithful_pilot_passes_and_the_adversary_condition_binds() {
        let c = cfg();
        let (children, parents, humans) = fixture(60, true);
        // R0's mirrors dropped the layout, so its before figure is large; the faithful pilot's is not.
        let v = pilot_gate_on(&children, &parents, &humans, &r0_excerpt_mirrors(60), &c).unwrap();
        assert_eq!(v["verdict"]["status"], "pass", "{}", v["verdict"]);
        let adv = &v["domains"]["news"]["adversary"];
        assert_eq!(v["domains"]["news"]["properties"]["trailing_ws_line_share"]["groups_nonzero"], 0);
        assert_eq!(adv["below_before"], true);
        assert_eq!((adv["humans"].as_u64(), adv["children"].as_u64(), adv["before_positives"].as_u64()), (Some(60), Some(60), Some(60)));
        assert_eq!(adv["origin_shares"], json!([0.2, 0.0, 0.8]));
        // Both scopes sit at a base rate of 0.8: the prior's Brier is 0.2 · 0.8² + 0.8 · 0.2² = 0.16.
        for k in ["pilot_prior_brier", "before_prior_brier"] {
            assert!((adv[k].as_f64().unwrap() - 0.16).abs() < 0.005, "{k} {}", adv[k]);
        }
        // "Below" is strict: R0 positives identical to the pilot's children give an equal before figure, which fails.
        let same: Vec<ARow> = children.iter().map(|k| positive(&format!("r0{}", k.id), 2, "local", "news", &k.group, k.text.clone())).collect();
        let eq = pilot_gate_on(&children, &parents, &humans, &same, &c).unwrap();
        let e = &eq["domains"]["news"]["adversary"];
        assert_eq!(e["pilot_gain_brier"], e["before_gain_brier"]);
        assert_eq!(eq["verdict"]["status"], "fail");
        // R0 positives of another source or origin are not a before figure: undetermined.
        let other: Vec<ARow> = r0_excerpt_mirrors(60).into_iter().map(|r| ARow { source: "frontier".into(), ..r }).collect();
        let u = pilot_gate_on(&children, &parents, &humans, &other, &c).unwrap();
        assert_eq!(u["verdict"]["status"], "undetermined");
        assert_eq!(u["domains"]["news"]["adversary"]["status"], "no_before_figure");
        let mixed: Vec<ARow> = r0_excerpt_mirrors(60).into_iter().map(|r| ARow { origin: 1, ..r }).collect();
        assert_eq!(pilot_gate_on(&children, &parents, &humans, &mixed, &c).unwrap()["verdict"]["status"], "undetermined");
        // Below the coverage floor nothing is gated: the verdict is a vacuous pass.
        let (c58, p58, h58) = fixture(58, false);
        let s = pilot_gate_on(&c58, &p58, &h58, &r0_excerpt_mirrors(58), &c).unwrap();
        assert_eq!((s["verdict"]["status"].as_str(), s["verdict"]["vacuous"].as_bool()), (Some("pass"), Some(true)));
        assert_eq!(s["bonferroni_cells"], 0);
        assert_eq!(s["domains"]["news"]["properties"]["trailing_ws_line_share"]["significant"], true, "reported at the uncorrected level");
    }

    /// The review's probe: R0's positives and the pilot's mirrors carry the
    /// same typography cue (an em dash in 3 of every 10) and copy their
    /// parents' layout, so no paired property differs.
    fn cue_text(g: usize, k: usize, cue: bool) -> String {
        let a = words(g, k);
        let b = words(g + 5, k + 1);
        if cue { format!("{a} \u{2014} {b}.\n{b}.") } else { format!("{a}, {b}.\n{b}.") }
    }

    fn probe(n: usize, pilot_cue: impl Fn(usize) -> bool) -> (Vec<Child>, HashMap<String, String>, Vec<ARow>, Vec<ARow>) {
        let mut children = Vec::new();
        let mut parents = HashMap::new();
        let mut humans = Vec::new();
        let mut r0 = Vec::new();
        for g in 0..n {
            let pid = format!("p{g}");
            let parent = cue_text(g, 1, false);
            parents.insert(pid.clone(), parent.clone());
            humans.push(human(&pid, "news", &format!("g{g}"), parent));
            r0.push(positive(&format!("r0m{g}"), 2, "local", "news", &format!("g{g}"), cue_text(g, 2, g % 10 < 3)));
            children.push(mirror(&format!("c{g}"), &pid, &format!("g{g}"), cue_text(g, 3, pilot_cue(g))));
        }
        (children, parents, humans, r0)
    }

    #[test]
    fn the_pilot_and_before_scopes_are_like_for_like() {
        let c = cfg();
        // The same cue rate in both: the figures agree closely (the review's
        // probe gave 0.038 against 0.013 when the pilot was weighted at its own base rate).
        let (children, parents, humans, r0) = probe(300, |g| g % 10 < 3);
        let v = pilot_gate_on(&children, &parents, &humans, &r0, &c).unwrap();
        let a = &v["domains"]["news"]["adversary"];
        let (p, b) = (a["pilot_gain_brier"].as_f64().unwrap(), a["before_gain_brier"].as_f64().unwrap());
        assert!(b > 0.005, "the cue is detectable: {b}");
        assert!((p - b).abs() < 0.02 * b, "pilot {p} before {b}");
        assert_eq!(v["bonferroni_cells"], 6);
        assert!(v["verdict"]["failures"].as_array().unwrap().iter().all(|f| f.as_str().unwrap().contains("adversary")), "no paired property differs: {}", v["verdict"]);
        // R0 positives of another source in the domain are left out of the before scope.
        let mut with_raid = r0.clone();
        with_raid.extend((0..300).map(|g| positive(&format!("raid{g}"), 2, "raid", "news", &format!("g{g}"), format!("{} \u{2014}\u{2014} x.  \n\n  y.", words(g, 9)))));
        let w = pilot_gate_on(&children, &parents, &humans, &with_raid, &c).unwrap();
        assert_eq!(w["domains"]["news"]["adversary"]["before_gain_brier"], a["before_gain_brier"]);
        assert_eq!(w["domains"]["news"]["adversary"]["before_positives"], 300);
        // A pilot without the cue passes; one carrying it in every mirror fails.
        let (c0, p0, h0, r00) = probe(300, |_| false);
        assert_eq!(pilot_gate_on(&c0, &p0, &h0, &r00, &c).unwrap()["verdict"]["status"], "pass");
        let (c1, p1, h1, r01) = probe(300, |_| true);
        assert_eq!(pilot_gate_on(&c1, &p1, &h1, &r01, &c).unwrap()["verdict"]["status"], "fail");
    }

    #[test]
    fn a_domain_with_a_cell_below_the_floor_is_not_gated() {
        // 60 mirrors and 30 mixed children: the mixed cell is below 59, so the domain is reported only.
        let (mut children, mut parents, mut humans) = fixture(60, false);
        for g in 60..90 {
            let parent = format!("{}. \n{}. \n{}.", words(g, 1), words(g, 2), words(g, 3));
            let pid = format!("p{g}");
            let s = parent.find(". \n").unwrap() + 3;
            let e = s + words(g, 2).len() + 1;
            let rep = format!("{}.", words(g, 7));
            let text = format!("{}{}{}", &parent[..s], rep, &parent[e..]);
            parents.insert(pid.clone(), parent.clone());
            humans.push(human(&pid, "news", &format!("g{g}"), parent));
            children.push(Child { id: format!("m{g}"), domain: "news".into(), origin: 1, parent_id: pid, group: format!("g{g}"), source: "local".into(), text, parent_spans: vec![(s, e)], final_spans: vec![(s, s + rep.len())] });
        }
        let mut r0 = r0_excerpt_mirrors(60);
        r0.extend((60..90).map(|g| positive(&format!("r0x{g}"), 1, "local", "news", &format!("g{g}"), excerpt(g))));
        let v = pilot_gate_on(&children, &parents, &humans, &r0, &cfg()).unwrap();
        assert_eq!(v["domains"]["news"]["cells"], json!({"mixed": 30, "model_only": 60}));
        assert_eq!(v["domains"]["news"]["gated"], false);
        assert_eq!(v["bonferroni_cells"], 0);
        assert_eq!((v["verdict"]["status"].as_str(), v["verdict"]["vacuous"].as_bool()), (Some("pass"), Some(true)));
        // Both origins in the before scope, weighted 0.2 / 0.55 / 0.25.
        assert_eq!(v["domains"]["news"]["adversary"]["origin_shares"], json!([0.2, 0.55, 0.25]));
    }

    #[test]
    fn the_adversary_reads_only_the_domains_humans() {
        let (children, parents, mut humans) = fixture(60, true);
        humans.extend((0..100).map(|g| human(&format!("s{g}"), "sports", &format!("s{g}"), format!("{}.\n\n{}.", words(g, 8), words(g, 9)))));
        let v = pilot_gate_on(&children, &parents, &humans, &r0_excerpt_mirrors(60), &cfg()).unwrap();
        assert_eq!(v["domains"]["news"]["adversary"]["humans"], 60);
        assert!(v["domains"]["sports"].is_null());
        assert_eq!(v["verdict"]["status"], "pass");
    }

    #[test]
    fn mixed_children_pair_against_the_replaced_lines() {
        let parent = "Alpha line one is here. \nBeta line two is here. \nGamma line three is here.";
        let s = parent.find("Beta").unwrap();
        let e = s + "Beta line two is here.".len();
        // Faithful: the replacement keeps the line's trailing whitespace (outside the span).
        let rep = "Delta line two was rewritten.";
        let child_text = format!("{}{}{}", &parent[..s], rep, &parent[e..]);
        let c = Child { id: "c".into(), domain: "d".into(), origin: 1, parent_id: "p".into(), group: "g".into(), source: "local".into(), text: child_text, parent_spans: vec![(s, e)], final_spans: vec![(s, s + rep.len())] };
        let (rc, rp) = pair_regions(&c, parent).unwrap();
        assert_eq!(rc, rp);
        assert_eq!((rp.lines, rp.trailing_ws_lines), (1, 1));
        // A replacement split over two lines adds a single break the parent
        // line lacks. `measure` reads it as a wrap (the text is then
        // hard-wrapped at W 18), so the wrap reading differs and no boundary
        // share does.
        let rep2 = "Delta line two was\nrewritten here.";
        let c2 = Child { text: format!("{}{}{}", &parent[..s], rep2, &parent[e..]), final_spans: vec![(s, s + rep2.len())], ..c.clone() };
        let (rc2, rp2) = pair_regions(&c2, parent).unwrap();
        assert_eq!((rc2.single_breaks, rc2.wrap_breaks, rc2.boundaries, rp2.boundaries), (1, 1, 0, 0));
        assert_eq!((rc2.hard_wrapped, rp2.hard_wrapped), (Some(true), Some(false)));
        assert_eq!(difference(&rc2, &rp2, "hard_wrapped"), 1.0);
        assert_eq!(difference(&rc2, &rp2, "single_newline_boundary_share"), 0.0);
        // A parent span off the parent's character boundaries is refused.
        let bad = Child { parent_spans: vec![(s, parent.len() + 1)], ..c };
        assert!(pair_regions(&bad, parent).is_err());
    }

    #[test]
    fn the_paired_test_alone_reports_without_a_verdict() {
        let (children, parents, _) = fixture(60, false);
        let v = paired_test_on(&children, &parents).unwrap();
        assert_eq!((v["schema"].as_str(), v["mode"].as_str()), (Some(PAIRED_SCHEMA), Some("paired_only")));
        assert_eq!(v["measure"], "layout_measure_v2");
        assert_eq!(v["report"]["status"], "reported");
        assert!(v["verdict"].is_null() && v["domains"]["news"]["adversary"].is_null());
        assert_eq!(v["domains"]["news"]["properties"]["trailing_ws_line_share"]["significant"], true);
        assert!(!v["report"]["significant_gated_cells"].as_array().unwrap().is_empty());
    }

    #[test]
    fn the_commands_resolve_children_against_their_parents_rows() {
        let dir = tempfile::tempdir().unwrap();
        let (children, _, humans) = fixture(60, true);
        let mut rows = String::new();
        for h in &humans {
            rows.push_str(&serde_json::to_string(&json!({"id": h.id, "text": h.text, "origin": "human_only", "domain": "news", "source_group": h.group, "source_kind": "human"})).unwrap());
            rows.push('\n');
        }
        for r in r0_excerpt_mirrors(60) {
            rows.push_str(&serde_json::to_string(&json!({"id": r.id, "text": r.text, "origin": "model_only", "domain": "news", "source_group": r.group, "source_kind": "local_positive"})).unwrap());
            rows.push('\n');
        }
        let train = dir.path().join("train.jsonl");
        std::fs::write(&train, rows).unwrap();
        // Children carry neither domain nor group: both come from the parents' rows.
        let mut pilot = String::new();
        for c in &children {
            pilot.push_str(&serde_json::to_string(&json!({"id": c.id, "origin": "model_only", "parent_id": c.parent_id, "text": c.text, "source_kind": "local_positive"})).unwrap());
            pilot.push('\n');
        }
        let pp = dir.path().join("pilot.jsonl");
        std::fs::write(&pp, pilot).unwrap();
        let s = pilot_gate(&PilotArgs { pilot: pp.clone(), parents: train.clone(), humans: train.clone(), before_rows: train.clone(), cfg: cfg(), out: dir.path().to_path_buf() }).unwrap();
        assert_eq!(s["status"], "pass");
        let v: Value = serde_json::from_str(&std::fs::read_to_string(dir.path().join("pilot-gate.json")).unwrap()).unwrap();
        assert_eq!((v["inputs"]["humans_rows"].as_u64(), v["inputs"]["before_rows_matched"].as_u64()), (Some(60), Some(60)));
        let t = paired_test(&PairedArgs { pilot: pp, parents: train, out: dir.path().to_path_buf() }).unwrap();
        assert_eq!(t["status"], "reported");
    }

    #[test]
    fn boundary_shares_and_the_wrap_reading_compare_like_for_like() {
        let r = |single: usize, blank: usize, hw: Option<bool>| Region {
            boundaries: single + blank,
            single_newline_boundaries: single,
            blank_line_runs: blank,
            hard_wrapped: hw,
            ..Region::default()
        };
        // Shares of boundaries, not counts: three blank runs against one are equal.
        assert_eq!(difference(&r(0, 3, None), &r(0, 1, None), "blank_line_run_boundary_share"), 0.0);
        assert!((difference(&r(1, 3, None), &r(0, 2, None), "blank_line_run_boundary_share") + 0.25).abs() < 1e-15);
        assert!((difference(&r(1, 3, None), &r(0, 2, None), "single_newline_boundary_share") - 0.25).abs() < 1e-15);
        // A region with no boundary reads 0 on either side: a lost boundary
        // counts against the parent's kind, an added one for the child's.
        assert_eq!(difference(&r(0, 0, None), &r(0, 2, None), "blank_line_run_boundary_share"), -1.0);
        assert_eq!(difference(&r(0, 0, None), &r(0, 2, None), "single_newline_boundary_share"), 0.0);
        assert_eq!(difference(&r(1, 0, None), &r(0, 0, None), "single_newline_boundary_share"), 1.0);
        // So does the modal separator share on the child side (0 when the parent has none).
        let parent = Region { separators: BTreeMap::from([("\\n\\n".to_string(), 2)]), ..r(0, 2, None) };
        assert_eq!(difference(&r(0, 0, None), &parent, SEPARATOR_PROPERTY), -1.0);
        assert_eq!(difference(&parent, &r(0, 0, None), SEPARATOR_PROPERTY), 0.0);
        // The wrap reading takes a difference only when both sides have one.
        assert_eq!(difference(&r(0, 0, None), &r(0, 0, Some(true)), "hard_wrapped"), 0.0);
        assert_eq!(difference(&r(0, 0, Some(false)), &r(0, 0, Some(true)), "hard_wrapped"), -1.0);
        assert_eq!(difference(&r(0, 0, Some(true)), &r(0, 0, Some(true)), "hard_wrapped"), 0.0);
        assert_eq!(PROPERTIES.len(), 6);
    }

    use grammar_a_form::transfer::{SourceLayout, transfer};

    /// Synthetic words for the layout tests below: a deterministic stream of
    /// syllable words (no corpus text).
    struct Stream(u64);
    impl Stream {
        fn below(&mut self, n: u64) -> u64 {
            self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (self.0 >> 33) % n
        }
        fn word(&mut self) -> String {
            const SYL: [&str; 12] = ["ba", "ko", "ri", "ten", "mol", "sa", "vi", "dor", "lu", "pe", "nar", "go"];
            let k = 1 + self.below(3);
            (0..k).map(|_| SYL[self.below(12) as usize]).collect()
        }
        /// One sentence of `n` words, capitalised, ending with `end`.
        fn sentence(&mut self, n: usize, end: &str) -> String {
            let mut w: Vec<String> = (0..n.max(1)).map(|_| self.word()).collect();
            w[0] = w[0][..1].to_uppercase() + &w[0][1..];
            format!("{}{end}", w.join(" "))
        }
        /// `n` words as 12-word sentences (the last shorter).
        fn prose(&mut self, n: usize) -> String {
            let mut out = Vec::new();
            let mut left = n.max(1);
            while left > 0 {
                out.push(self.sentence(left.min(12), "."));
                left -= left.min(12);
            }
            out.join(" ")
        }
    }

    fn wrapped(p: &str, w: usize) -> String {
        grammar_a_form::transfer::wrap(p, w, 0).join("\n")
    }

    /// Children of one domain, `d`, each its own source group.
    #[derive(Default)]
    struct Pilot {
        children: Vec<Child>,
        parents: HashMap<String, String>,
    }

    impl Pilot {
        fn add(&mut self, parent: String, origin: usize, text: String, parent_spans: Spans, final_spans: Spans) {
            let g = self.children.len();
            self.parents.insert(format!("p{g}"), parent);
            self.children.push(Child {
                id: format!("c{g}"),
                domain: "d".into(),
                origin,
                parent_id: format!("p{g}"),
                group: format!("g{g}"),
                source: "local".into(),
                text,
                parent_spans,
                final_spans,
            });
        }
        /// A mirror laid out by the landed transfer (identity conventions).
        fn mirror(&mut self, parent: String, raw: &str) {
            let text = transfer(raw, &crate::layout_audit::identity_conventions(), &SourceLayout::whole(&parent)).text;
            self.add(parent, 2, text, vec![], vec![]);
        }
        /// A mixed child: `parent[s..e]` replaced by `raw` laid out by the
        /// landed transfer, or spliced as it is.
        fn mixed(&mut self, parent: String, (s, e): (usize, usize), raw: &str, transferred: bool) {
            let rep = if transferred {
                transfer(raw, &crate::layout_audit::identity_conventions(), &SourceLayout::span(&parent, s, e).unwrap()).text
            } else {
                raw.to_string()
            };
            let text = format!("{}{rep}{}", &parent[..s], &parent[e..]);
            self.add(parent, 1, text, vec![(s, e)], vec![(s, s + rep.len())]);
        }
        /// The properties significant in `d` (gated at 60 groups, K = 6).
        fn significant(&self) -> Vec<String> {
            let t = paired_test_on(&self.children, &self.parents).unwrap();
            assert_eq!((t["domains"]["d"]["gated"].as_bool(), t["bonferroni_cells"].as_u64()), (Some(true), Some(6)));
            PROPERTIES.iter().filter(|p| t["domains"]["d"]["properties"][**p]["significant"] == true).map(|p| p.to_string()).collect()
        }
        /// Per pair, the sign of child − parent in a raw region count.
        fn count_signs(&self, count: impl Fn(&Region) -> usize) -> (usize, usize) {
            let mut out = (0, 0);
            for c in &self.children {
                let (rc, rp) = pair_regions(c, &self.parents[&c.parent_id]).unwrap();
                out.0 += (count(&rc) > count(&rp)) as usize;
                out.1 += (count(&rc) < count(&rp)) as usize;
            }
            out
        }
    }

    /// Hard-wrapped two-paragraph parents (W 62, 50-89 words a paragraph) and
    /// mirrors at `pct` of each paragraph's words, laid out by the transfer.
    fn hard_wrapped_mirrors(pct: usize, seed: u64) -> Pilot {
        let mut s = Stream(seed);
        let mut p = Pilot::default();
        for _ in 0..60 {
            let (n1, n2) = (50 + s.below(40) as usize, 50 + s.below(40) as usize);
            let parent = format!("{}\n\n{}", wrapped(&s.prose(n1), 62), wrapped(&s.prose(n2), 62));
            let raw = format!("{}\n\n{}", s.prose(n1 * pct / 100), s.prose(n2 * pct / 100));
            p.mirror(parent, &raw);
        }
        p
    }

    #[test]
    fn children_carrying_their_parents_layout_pass_at_any_length() {
        // The review's counterexample (B12 landing review r1, protocol
        // finding 1): hard-wrapped mirrors at 80% and 125% of their parents'
        // words. The raw break counts move with length in every pair; the
        // amended properties do not.
        for pct in [80, 100, 125] {
            let p = hard_wrapped_mirrors(pct, 4242);
            assert!(p.significant().is_empty(), "{pct}%: {:?}", p.significant());
            // The raw count's sign test would fail at either length.
            let (more, less) = p.count_signs(|r| r.single_breaks);
            assert_eq!(sign_test_p(more, less) < bonferroni(6), pct != 100, "{pct}%: +{more} -{less}");
        }
        // Line-form mirrors at 150% of their parents' lines: their own lines
        // (content) take the parent's separators, one newline or a blank line.
        for sep in ["\n", "\n\n"] {
            let mut s = Stream(99);
            let mut p = Pilot::default();
            for _ in 0..60 {
                let n = 8 + s.below(12) as usize;
                let line = |s: &mut Stream| {
                    let k = 2 + s.below(5) as usize;
                    if sep == "\n" { format!("<{}> {}", s.word(), s.sentence(k, "")) } else { format!("- {}", s.sentence(k, "")) }
                };
                let parent = (0..n).map(|_| line(&mut s)).collect::<Vec<_>>().join(sep);
                let raw = (0..n * 3 / 2).map(|_| line(&mut s)).collect::<Vec<_>>().join("\n");
                p.mirror(parent, &raw);
            }
            assert!(p.significant().is_empty(), "{sep:?}: {:?}", p.significant());
            let (more, less) = p.count_signs(|r| r.single_breaks + r.blank_line_runs);
            assert_eq!((more, less), (60, 0));
        }
        // Mirrors at 80% of hard-wrapped parents whose middle paragraph is one
        // unwrapped line: the whole text's wrap reading is W alone, so a
        // shorter copy of that line that fits W changes nothing.
        // The same for mixed children replacing that line: a span region of
        // short lines has no wrap reading.
        let mut s = Stream(77);
        let (mut p, mut q) = (Pilot::default(), Pilot::default());
        for _ in 0..60 {
            let (n1, n2, n3) = (50 + s.below(30) as usize, 14 + s.below(10) as usize, 50 + s.below(30) as usize);
            let (a, b, c) = (wrapped(&s.prose(n1), 62), s.prose(n2), wrapped(&s.prose(n3), 62));
            let parent = format!("{a}\n\n{b}\n\n{c}");
            let raw = format!("{}\n\n{}\n\n{}", s.prose(n1 * 4 / 5), s.prose(n2 * 4 / 5), s.prose(n3 * 4 / 5));
            p.mirror(parent.clone(), &raw);
            let span = s.prose(n2 * 4 / 5);
            q.mixed(parent, (a.len() + 2, a.len() + 2 + b.len()), &span, true);
        }
        assert!(p.significant().is_empty(), "unwrapped line, mirrors: {:?}", p.significant());
        assert!(q.significant().is_empty(), "unwrapped line, mixed: {:?}", q.significant());
        // Mixed children replacing a whole hard-wrapped paragraph, or its last
        // sentence, at half and twice the replaced words.
        for pct in [50, 200] {
            for whole_paragraph in [true, false] {
                let mut s = Stream(1000 + pct as u64);
                let mut p = Pilot::default();
                for _ in 0..60 {
                    let s1: Vec<String> = (0..4).map(|_| s.sentence(12, ".")).collect();
                    let (w1, w2) = (wrapped(&s1.join(" "), 62), wrapped(&s.prose(48), 62));
                    let (start, words) = if whole_paragraph { (0, 48) } else { (w1.len() - s1[3].len(), 12) };
                    let raw = s.prose(words * pct / 100);
                    let end = w1.len();
                    p.mixed(format!("{w1}\n\n{w2}"), (start, end), &raw, true);
                }
                assert!(p.significant().is_empty(), "{pct}% whole paragraph {whole_paragraph}: {:?}", p.significant());
            }
        }
    }

    #[test]
    fn layout_faults_are_still_seen() {
        let mut s = Stream(808);
        let mut faults: BTreeMap<&str, Pilot> = BTreeMap::new();
        for _ in 0..60 {
            let (a, b, ra, rb) = (s.prose(60), s.prose(70), s.prose(60), s.prose(70));
            let hw = format!("{}\n\n{}", wrapped(&a, 62), wrapped(&b, 62));
            // A mirror left unwrapped, single-newline paragraphs made blank-line
            // ones and the reverse, and two paragraphs joined into one.
            faults.entry("unwrapped").or_default().add(hw.clone(), 2, format!("{ra}\n\n{rb}"), vec![], vec![]);
            faults.entry("blank_for_single").or_default().add(format!("{a}\n{b}"), 2, format!("{ra}\n\n{rb}"), vec![], vec![]);
            faults.entry("single_for_blank").or_default().add(format!("{a}\n\n{b}"), 2, format!("{ra}\n{rb}"), vec![], vec![]);
            faults.entry("joined").or_default().add(hw.clone(), 2, wrapped(&format!("{ra} {rb}"), 62), vec![], vec![]);
            // A mixed child whose replacement of a hard-wrapped paragraph is spliced unwrapped.
            let w1 = wrapped(&a, 62);
            faults.entry("span_unwrapped").or_default().mixed(hw.clone(), (0, w1.len()), &ra, false);
        }
        let sig = |k: &str| faults[k].significant();
        assert_eq!(sig("unwrapped"), vec!["hard_wrapped"]);
        assert_eq!(sig("blank_for_single"), vec!["single_newline_boundary_share", "blank_line_run_boundary_share", SEPARATOR_PROPERTY]);
        assert_eq!(sig("single_for_blank"), vec!["single_newline_boundary_share", "blank_line_run_boundary_share", SEPARATOR_PROPERTY]);
        assert_eq!(sig("joined"), vec!["blank_line_run_boundary_share", SEPARATOR_PROPERTY]);
        assert_eq!(sig("span_unwrapped"), vec!["hard_wrapped"]);
    }

    #[test]
    fn a_span_ending_inside_a_hard_wrapped_paragraph_is_seen() {
        // Amendment 16 item 16.3 (recorded, not decided): the parent
        // text after the span keeps its own line breaks, so the line where the
        // child's text meets it can be shorter than the wrapper fills, and
        // `measure` reads the break after it as a sentence break: a boundary
        // the parent region lacks. The gate sees it with the replaced
        // sentence's own word count.
        let mut s = Stream(1100);
        let mut p = Pilot::default();
        for _ in 0..60 {
            let s1: Vec<String> = (0..4).map(|_| s.sentence(12, ".")).collect();
            let (w1, w2) = (wrapped(&s1.join(" "), 62), wrapped(&s.prose(48), 62));
            let start = s1[0].len() + 1;
            let raw = s.prose(12);
            p.mixed(format!("{w1}\n\n{w2}"), (start, start + s1[1].len()), &raw, true);
        }
        assert_eq!(p.significant(), vec!["single_newline_boundary_share"]);
        let t = paired_test_on(&p.children, &p.parents).unwrap();
        assert_eq!(t["domains"]["d"]["properties"]["single_newline_boundary_share"]["negative"], 0);
    }
}
