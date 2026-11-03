//! Ungated competitive-A diagnostics beside the protocol metrics:
//!
//! - **Answer consistency.** Exact repeats and lightly perturbed copies of
//!   evaluation documents (probe manifest `slop_ninja_a_consistency_probe_v1`,
//!   written by `slop_ninja_a_augment probes`) are scored against the base
//!   document's prediction: the change in P(involvement) and in share, how
//!   many repeats are bit-identical, and how many decisions flip at 0.5 and at
//!   the seed's operating points.
//! - **Out-of-domain human FPR.** Humans of the evaluated split (every
//!   population, diagnostic ones included) whose domain has no Train humans
//!   or no Train rows at all, or is declared out of domain: FPR at 0.5 and at
//!   the operating points with one-sided Clopper-Pearson bounds, beside the
//!   in-domain humans of the same split.
//!
//! Neither enters the primary metric, a gate or the Pangram strata. Like the
//! rest of the evaluator this reads metadata and predictions only, never a
//! text field.
//!
//! Attribution: re-querying a detector on repeated texts and keeping an
//! out-of-domain human check follow ideas in the MIT-licensed It's AI
//! validator (<https://github.com/It-s-AI/llm-detection>, Copyright 2024
//! It's AI). Here both are reported diagnostics with intervals, not
//! penalties; the probe format, statistics and domain rule are our own.
use crate::comp_eval::{Obs, Row, bootstrap_draws};
use crate::pangram_subsample::clopper_pearson_upper;
use anyhow::{Context, Result, bail, ensure};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use std::collections::{BTreeMap, BTreeSet};

pub const PROBE_SCHEMA: &str = "slop_ninja_a_consistency_probe_v1";
pub const CONSISTENCY_SCHEMA: &str = "slop_ninja_a_comp_consistency_v1";
pub const OOD_SCHEMA: &str = "slop_ninja_a_comp_ood_human_fpr_v1";
/// Absolute-change thresholds reported for P(involvement).
pub const DELTA_LEVELS: [f64; 3] = [0.01, 0.05, 0.10];

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum ProbeKind {
    Repeat,
    Perturbed,
}

impl ProbeKind {
    pub fn label(self) -> &'static str {
        match self {
            ProbeKind::Repeat => "repeat",
            ProbeKind::Perturbed => "perturbed",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Probe {
    pub probe_id: String,
    pub base_id: String,
    pub kind: ProbeKind,
    pub profile: Option<String>,
    /// Operations with at least one edit (perturbed probes).
    pub ops: Vec<String>,
    pub word_edits: u64,
    pub text_changed: bool,
}

#[derive(Deserialize)]
struct RawProbe {
    schema: String,
    probe_id: String,
    base_id: String,
    #[serde(default)]
    split: Option<String>,
    kind: String,
    base_text_sha256: String,
    text_sha256: String,
    #[serde(default)]
    profile: Option<String>,
    #[serde(default)]
    edits_by_operation: Option<Map<String, Value>>,
    #[serde(default)]
    word_edits: Option<u64>,
    #[serde(default)]
    gap_edits: Option<u64>,
}

/// Parse and check a probe manifest against the evaluated rows: unique probe
/// ids that are not row ids, a base document of the split, repeats with
/// identical digests and no edits, perturbations with a noise profile.
pub fn parse_probes(jsonl: &str, rows: &[Row]) -> Result<Vec<Probe>> {
    let split = rows.first().map(|r| r.split.clone()).unwrap_or_default();
    let row_ids: BTreeSet<&str> = rows.iter().map(|r| r.id.as_str()).collect();
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for (i, line) in jsonl.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let r: RawProbe = serde_json::from_str(line).with_context(|| format!("probes line {}", i + 1))?;
        ensure!(r.schema == PROBE_SCHEMA, "probes line {}: schema {}", i + 1, r.schema);
        ensure!(seen.insert(r.probe_id.clone()), "duplicate probe id {}", r.probe_id);
        ensure!(!row_ids.contains(r.probe_id.as_str()), "probe id {} is also a document id", r.probe_id);
        ensure!(row_ids.contains(r.base_id.as_str()), "probe {} names base {} outside the evaluated split", r.probe_id, r.base_id);
        if let Some(s) = &r.split {
            ensure!(*s == split, "probe {} is for split {s}, evaluating {split}", r.probe_id);
        }
        let ops: Vec<String> = r
            .edits_by_operation
            .as_ref()
            .map(|m| {
                m.iter()
                    .filter(|(_, v)| v["human"].as_u64().unwrap_or(0) + v["model"].as_u64().unwrap_or(0) > 0)
                    .map(|(k, _)| k.clone())
                    .collect()
            })
            .unwrap_or_default();
        let kind = match r.kind.as_str() {
            "repeat" => {
                ensure!(r.text_sha256 == r.base_text_sha256, "repeat probe {} has a different text digest", r.probe_id);
                ensure!(ops.is_empty() && r.word_edits.unwrap_or(0) == 0 && r.gap_edits.unwrap_or(0) == 0, "repeat probe {} records edits", r.probe_id);
                ProbeKind::Repeat
            }
            "perturbed" => {
                ensure!(r.profile.is_some(), "perturbed probe {} names no noise profile", r.probe_id);
                ProbeKind::Perturbed
            }
            k => bail!("probe {}: unknown kind {k}", r.probe_id),
        };
        out.push(Probe {
            text_changed: r.text_sha256 != r.base_text_sha256,
            probe_id: r.probe_id,
            base_id: r.base_id,
            kind,
            profile: r.profile,
            ops,
            word_edits: r.word_edits.unwrap_or(0),
        });
    }
    Ok(out)
}

/// P(involvement) of an observation (`None` for an execution failure).
fn involvement(o: &Obs) -> Option<(f64, f64)> {
    match o {
        Obs::Ok(p) => Some((p.p_model_only + p.p_mixed, p.share)),
        Obs::Failed(_) => None,
    }
}

/// Nearest-rank quantile of sorted values.
fn quantile(sorted: &[f64], q: f64) -> Option<f64> {
    if sorted.is_empty() {
        return None;
    }
    let k = ((q * sorted.len() as f64).ceil() as usize).clamp(1, sorted.len());
    Some(sorted[k - 1])
}

struct Pair<'a> {
    group: &'a str,
    base: f64,
    probe: f64,
    dshare: f64,
}

fn summarize(pairs: &[Pair], thresholds: &[(String, f64)]) -> Value {
    let n = pairs.len();
    if n == 0 {
        return json!({"n": 0});
    }
    let d: Vec<f64> = pairs.iter().map(|p| p.probe - p.base).collect();
    let mut a: Vec<f64> = d.iter().map(|x| x.abs()).collect();
    a.sort_by(f64::total_cmp);
    let mean = d.iter().sum::<f64>() / n as f64;
    let identical = pairs.iter().filter(|p| p.base.to_bits() == p.probe.to_bits()).count();
    let two_dp = pairs.iter().filter(|p| (p.base * 100.0).round() == (p.probe * 100.0).round()).count();
    let mut levels = Map::new();
    for l in DELTA_LEVELS {
        levels.insert(format!("{l}"), json!(a.iter().filter(|x| **x >= l).count()));
    }
    let mut flips = Map::new();
    for (label, t) in thresholds {
        let newly = pairs.iter().filter(|p| p.base < *t && p.probe >= *t).count();
        let un = pairs.iter().filter(|p| p.base >= *t && p.probe < *t).count();
        flips.insert(label.clone(), json!({"threshold": if t.is_finite() { json!(t) } else { json!("inf") },
            "base_flagged": pairs.iter().filter(|p| p.base >= *t).count(), "probe_flagged": pairs.iter().filter(|p| p.probe >= *t).count(),
            "flips": newly + un, "newly_flagged": newly, "newly_unflagged": un}));
    }
    // source-group bootstrap of the mean signed change (ratio over groups)
    let mut per: BTreeMap<&str, (f64, f64)> = BTreeMap::new();
    for (p, x) in pairs.iter().zip(&d) {
        let e = per.entry(p.group).or_default();
        e.0 += x;
        e.1 += 1.0;
    }
    let units: Vec<(f64, f64)> = per.into_values().collect();
    let ci = bootstrap_draws(units.len(), |ix| {
        let (s, c) = ix.iter().fold((0.0, 0.0), |acc, &j| (acc.0 + units[j].0, acc.1 + units[j].1));
        s / c
    })
    .map(|v| json!([v[24], v[974]]));
    json!({
        "n": n,
        "source_groups": units.len(),
        "mean_delta_involvement": mean,
        "mean_delta_involvement_source_group_bootstrap_95": ci,
        "mean_abs_delta_involvement": a.iter().sum::<f64>() / n as f64,
        "max_abs_delta_involvement": a[n - 1],
        "abs_delta_quantiles": {"p50": quantile(&a, 0.5), "p90": quantile(&a, 0.9), "p99": quantile(&a, 0.99)},
        "bit_identical_involvement": identical,
        "equal_at_2_decimals": two_dp,
        "abs_delta_at_least": levels,
        "mean_abs_delta_share": pairs.iter().map(|p| p.dshare.abs()).sum::<f64>() / n as f64,
        "decision_flips": flips,
    })
}

/// Consistency of one system fit on the probes. `thresholds` are labelled
/// decision thresholds (0.5 and the seed's operating points). A probe whose
/// base or probe observation is missing or failed is counted, not scored.
pub fn consistency(
    rows: &[Row],
    probes: &[Probe],
    base: &BTreeMap<String, Obs>,
    probe_preds: &BTreeMap<String, Obs>,
    thresholds: &[(String, f64)],
) -> Result<Value> {
    let by_id: BTreeMap<&str, &Row> = rows.iter().map(|r| (r.id.as_str(), r)).collect();
    let known: BTreeSet<&str> = probes.iter().map(|p| p.probe_id.as_str()).collect();
    for k in probe_preds.keys() {
        ensure!(known.contains(k.as_str()), "probe prediction {k} names no probe");
    }
    let origins = ["human_only", "model_only", "mixed"];
    let mut groups: BTreeMap<(ProbeKind, String), Vec<Pair>> = BTreeMap::new();
    let mut accounting: BTreeMap<&'static str, BTreeMap<&'static str, usize>> = BTreeMap::new();
    let mut unchanged = 0usize;
    for p in probes {
        let row = by_id[p.base_id.as_str()];
        let acc = accounting.entry(p.kind.label()).or_default();
        *acc.entry("probes").or_default() += 1;
        if p.kind == ProbeKind::Perturbed && !p.text_changed {
            unchanged += 1;
        }
        let b = match base.get(&p.base_id) {
            None => {
                *acc.entry("base_missing").or_default() += 1;
                continue;
            }
            Some(o) => involvement(o),
        };
        let q = match probe_preds.get(&p.probe_id) {
            None => {
                *acc.entry("probe_missing").or_default() += 1;
                continue;
            }
            Some(o) => involvement(o),
        };
        let (Some(b), Some(q)) = (b, q) else {
            *acc.entry("execution_failure").or_default() += 1;
            continue;
        };
        *acc.entry("scored").or_default() += 1;
        let mk = || Pair { group: row.group.as_str(), base: b.0, probe: q.0, dshare: q.1 - b.1 };
        groups.entry((p.kind, "all".into())).or_default().push(mk());
        groups.entry((p.kind, format!("origin:{}", origins[row.origin]))).or_default().push(mk());
        if p.kind == ProbeKind::Perturbed {
            for op in &p.ops {
                groups.entry((p.kind, format!("operation:{op}"))).or_default().push(mk());
            }
            if let Some(prof) = &p.profile {
                groups.entry((p.kind, format!("profile:{prof}"))).or_default().push(mk());
            }
        }
    }
    let mut out = Map::new();
    for kind in [ProbeKind::Repeat, ProbeKind::Perturbed] {
        let mut m = Map::new();
        for ((k, g), v) in &groups {
            if *k == kind {
                m.insert(g.clone(), summarize(v, thresholds));
            }
        }
        out.insert(kind.label().into(), Value::Object(m));
    }
    let repeats = groups.get(&(ProbeKind::Repeat, "all".to_string()));
    let all_identical = repeats.map(|v| v.iter().all(|p| p.base.to_bits() == p.probe.to_bits()));
    Ok(json!({
        "schema": CONSISTENCY_SCHEMA,
        "status": "diagnostic_ungated",
        "accounting": accounting,
        "perturbed_probes_with_unchanged_text": unchanged,
        "repeats_all_bit_identical": all_identical,
        "delta": "probe minus base P(involvement) = p_model_only + p_mixed; share delta likewise",
        "summaries": out,
    }))
}

/// Domain rule for the out-of-domain human slice.
#[derive(Clone, Debug, Default)]
pub struct DomainRule {
    /// Train domain to whether it has Train humans (from the Train
    /// MANIFEST `counts_domain_origin_generator`).
    pub train: BTreeMap<String, bool>,
    /// Domains declared out of domain whatever Train holds.
    pub declared: BTreeSet<String>,
}

impl DomainRule {
    /// Read `counts_domain_origin_generator` ({domain: {origin: {label: n}}})
    /// from a Train bundle MANIFEST.json.
    pub fn from_train_manifest(m: &Value) -> Result<DomainRule> {
        ensure!(m["split"] == "train", "--train-manifest must be the Train split MANIFEST (split is {})", m["split"]);
        let c = m["counts_domain_origin_generator"].as_object().context("MANIFEST counts_domain_origin_generator")?;
        let train = c
            .iter()
            .map(|(d, v)| {
                let humans = v["human_only"].as_object().is_some_and(|x| x.values().any(|n| n.as_u64().unwrap_or(0) > 0));
                (d.clone(), humans)
            })
            .collect();
        Ok(DomainRule { train, declared: BTreeSet::new() })
    }

    /// Why a domain is out of domain (`None`: in domain).
    pub fn reason(&self, domain: &str) -> Option<&'static str> {
        if self.declared.contains(domain) {
            return Some("declared");
        }
        match self.train.get(domain) {
            None => Some("domain_absent_from_train"),
            Some(false) => Some("domain_without_train_humans"),
            Some(true) => None,
        }
    }
}

fn rate_block(flags: &[bool], fails: usize) -> Value {
    let n = flags.len();
    let k = flags.iter().filter(|x| **x).count();
    json!({"count": k, "n": n, "rate": (n > 0).then(|| k as f64 / n as f64),
        "clopper_pearson_upper_one_sided_95": (n > 0).then(|| clopper_pearson_upper(k as u64, n as u64, 0.95)),
        "execution_failures_counted_as_flagged": fails})
}

/// Human FPR on out-of-domain and in-domain humans of the split for one
/// system fit. A missing or failed observation counts as flagged (the
/// evaluator's execution-failure rule).
pub fn ood_human_fpr(rows: &[Row], preds: &BTreeMap<String, Obs>, rule: &DomainRule, thresholds: &[(String, f64)]) -> Value {
    let mut ood: Vec<(&Row, f64, bool, &'static str)> = Vec::new();
    let mut ind: Vec<(&Row, f64, bool)> = Vec::new();
    for r in rows.iter().filter(|r| r.is_human()) {
        let (inv, failed) = match preds.get(&r.id).and_then(involvement) {
            Some((v, _)) => (v, false),
            None => (1.0, true),
        };
        match rule.reason(&r.domain) {
            Some(why) => ood.push((r, inv, failed, why)),
            None => ind.push((r, inv, failed)),
        }
    }
    let block = |items: &[(f64, bool)]| -> Value {
        let fails = items.iter().filter(|x| x.1).count();
        let mut m = Map::new();
        for (label, t) in thresholds {
            let f: Vec<bool> = items.iter().map(|x| x.0 >= *t).collect();
            m.insert(label.clone(), rate_block(&f, fails));
        }
        let inv: Vec<f64> = items.iter().map(|x| x.0).collect();
        json!({"humans": items.len(), "mean_involvement": (!inv.is_empty()).then(|| inv.iter().sum::<f64>() / inv.len() as f64), "fpr": m})
    };
    let all_ood: Vec<(f64, bool)> = ood.iter().map(|x| (x.1, x.2)).collect();
    let all_ind: Vec<(f64, bool)> = ind.iter().map(|x| (x.1, x.2)).collect();
    let mut by_domain: BTreeMap<String, Vec<(f64, bool)>> = BTreeMap::new();
    let mut by_reason: BTreeMap<&str, Vec<(f64, bool)>> = BTreeMap::new();
    let mut by_pop: BTreeMap<String, Vec<(f64, bool)>> = BTreeMap::new();
    let mut groups = BTreeSet::new();
    for (r, v, f, why) in &ood {
        by_domain.entry(r.domain.clone()).or_default().push((*v, *f));
        by_reason.entry(why).or_default().push((*v, *f));
        by_pop.entry(r.population.clone()).or_default().push((*v, *f));
        groups.insert(r.group.as_str());
    }
    let reasons: BTreeMap<String, &str> = ood.iter().map(|x| (x.0.domain.clone(), x.3)).collect();
    json!({
        "schema": OOD_SCHEMA,
        "status": "diagnostic_ungated",
        "rule": "humans of the evaluated split, every population, whose domain has no Train rows, has Train positives but no Train humans, or is declared out of domain",
        "out_of_domain": block(&all_ood),
        "out_of_domain_source_groups": groups.len(),
        "in_domain": block(&all_ind),
        "domain_reasons": reasons,
        "by_reason": by_reason.iter().map(|(k, v)| (k.to_string(), block(v))).collect::<Map<String, Value>>(),
        "by_domain": by_domain.iter().map(|(k, v)| (k.clone(), block(v))).collect::<Map<String, Value>>(),
        "by_population": by_pop.iter().map(|(k, v)| (k.clone(), block(v))).collect::<Map<String, Value>>(),
    })
}

/// Read the labelled thresholds of one seed from an evaluation document:
/// 0.5 plus every operating point (exact f64 bits).
pub fn seed_thresholds(eval: &Value, system: &str, seed: u64) -> Result<Vec<(String, f64)>> {
    let mut v = vec![("0.5".to_string(), 0.5)];
    let ops = &eval["systems"][system]["seeds"][seed.to_string()]["operating_points"];
    if let Some(m) = ops.as_object() {
        for (label, o) in m {
            let bits = o["threshold"]["bits"].as_str().with_context(|| format!("{system}:{seed} operating point {label} bits"))?;
            let b = u64::from_str_radix(bits.trim_start_matches("0x"), 16).context("threshold bits")?;
            v.push((format!("op_{label}"), f64::from_bits(b)));
        }
    }
    Ok(v)
}

fn fnum(v: &Value, d: usize) -> String {
    match v.as_f64() {
        Some(x) => format!("{x:.d$}"),
        None => "-".into(),
    }
}

/// Markdown section for `evaluation.md`.
pub fn markdown(diag: &Value) -> String {
    let mut s = String::new();
    if let Some(c) = diag["consistency"].as_object().filter(|c| !c.is_empty()) {
        s.push_str("\n## Answer consistency (ungated diagnostic)\n\n");
        s.push_str("| System:seed | Kind | Slice | n | mean Δ | 95% CI | mean \\|Δ\\| | max \\|Δ\\| | bit-identical | flips at 0.5 |\n|---|---|---|---:|---:|---|---:|---:|---:|---:|\n");
        for (k, v) in c {
            for kind in ["repeat", "perturbed"] {
                if let Some(m) = v["summaries"][kind].as_object() {
                    for (g, x) in m {
                        if !(g == "all" || g.starts_with("origin:")) {
                            continue;
                        }
                        let ci = x["mean_delta_involvement_source_group_bootstrap_95"]
                            .as_array()
                            .map(|a| format!("[{}, {}]", fnum(&a[0], 4), fnum(&a[1], 4)))
                            .unwrap_or_else(|| "-".into());
                        s.push_str(&format!(
                            "| {k} | {kind} | {g} | {} | {} | {ci} | {} | {} | {} | {} |\n",
                            x["n"],
                            fnum(&x["mean_delta_involvement"], 4),
                            fnum(&x["mean_abs_delta_involvement"], 4),
                            fnum(&x["max_abs_delta_involvement"], 4),
                            x["bit_identical_involvement"],
                            x["decision_flips"]["0.5"]["flips"],
                        ));
                    }
                }
            }
        }
    }
    if let Some(o) = diag["out_of_domain_humans"].as_object().filter(|c| !c.is_empty()) {
        s.push_str("\n## Out-of-domain human FPR (ungated diagnostic)\n\n");
        s.push_str("| System:seed | Humans | OOD humans | OOD FPR at 0.5 | upper 95% | In-domain FPR at 0.5 |\n|---|---|---:|---:|---:|---:|\n");
        for (k, v) in o {
            let a = &v["out_of_domain"];
            let b = &v["in_domain"];
            s.push_str(&format!(
                "| {k} | {} | {} | {} | {} | {} |\n",
                a["humans"].as_u64().unwrap_or(0) + b["humans"].as_u64().unwrap_or(0),
                a["humans"],
                fnum(&a["fpr"]["0.5"]["rate"], 4),
                fnum(&a["fpr"]["0.5"]["clopper_pearson_upper_one_sided_95"], 4),
                fnum(&b["fpr"]["0.5"]["rate"], 4),
            ));
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::comp_eval::{Prediction, parse_rows};

    fn rows() -> Vec<Row> {
        let mut s = String::new();
        let mk = |id: &str, origin: &str, share: f64, label: Option<&str>, group: &str, domain: &str, pop: &str| {
            json!({"id": id, "split": "dev_iter", "origin": origin, "share": share, "generator_label": label,
                "source_group": group, "domain": domain, "population": pop, "operation": if origin == "human_only" { "human" } else { "mirror" }})
        };
        for r in [
            mk("h1", "human_only", 0.0, None, "g1", "wikipedia", "primary"),
            mk("h2", "human_only", 0.0, None, "g2", "wikipedia", "primary"),
            mk("h3", "human_only", 0.0, None, "g3", "spoken", "sbcsae_spoken_conversation"),
            mk("h4", "human_only", 0.0, None, "g4", "newdomain", "primary"),
            mk("m1", "model_only", 1.0, Some("qwen3.6"), "g1", "wikipedia", "primary"),
            mk("m2", "model_only", 1.0, Some("qwen3.6"), "g2", "wikipedia", "primary"),
        ] {
            s.push_str(&serde_json::to_string(&r).unwrap());
            s.push('\n');
        }
        parse_rows(&s).unwrap()
    }

    fn obs(inv: f64) -> Obs {
        Obs::Ok(Prediction {
            p_human: 1.0 - inv,
            p_model_only: inv,
            p_mixed: 0.0,
            share: inv,
            attribution: BTreeMap::new(),
            roles: BTreeMap::new(),
        })
    }

    fn probe_line(pid: &str, base: &str, kind: &str, changed: bool, ops: Value) -> String {
        let t = if changed { "bb" } else { "aa" };
        serde_json::to_string(&json!({"schema": PROBE_SCHEMA, "probe_id": pid, "base_id": base, "split": "dev_iter", "kind": kind,
            "base_text_sha256": "aa", "text_sha256": t, "profile": if kind == "perturbed" { json!("light_v1") } else { Value::Null },
            "edits_by_operation": ops, "word_edits": if kind == "repeat" { 0 } else { 1 }, "gap_edits": 0}))
        .unwrap()
    }

    fn manifest() -> String {
        [
            probe_line("p:r:h1", "h1", "repeat", false, json!({})),
            probe_line("p:r:m1", "m1", "repeat", false, json!({})),
            probe_line("p:p:h1", "h1", "perturbed", true, json!({"typo": {"human": 1, "model": 0}})),
            probe_line("p:p:h2", "h2", "perturbed", true, json!({"casing": {"human": 1, "model": 0}})),
            probe_line("p:p:m2", "m2", "perturbed", true, json!({"typo": {"human": 0, "model": 1}})),
        ]
        .join("\n")
    }

    #[test]
    fn probe_manifest_is_validated() {
        let r = rows();
        let p = parse_probes(&manifest(), &r).unwrap();
        assert_eq!(p.len(), 5);
        assert_eq!(p[2].ops, vec!["typo"]);
        let bad_repeat = probe_line("x", "h1", "repeat", true, json!({}));
        assert!(parse_probes(&bad_repeat, &r).is_err());
        assert!(parse_probes(&probe_line("x", "nope", "perturbed", true, json!({})), &r).is_err());
        assert!(parse_probes(&probe_line("h2", "h1", "perturbed", true, json!({})), &r).is_err());
        let dup = format!("{}\n{}", probe_line("x", "h1", "perturbed", true, json!({})), probe_line("x", "h2", "perturbed", true, json!({})));
        assert!(parse_probes(&dup, &r).is_err());
        let wrong = probe_line("x", "h1", "perturbed", true, json!({})).replace(PROBE_SCHEMA, "other");
        assert!(parse_probes(&wrong, &r).is_err());
    }

    #[test]
    fn consistency_deltas_flips_and_accounting() {
        let r = rows();
        let p = parse_probes(&manifest(), &r).unwrap();
        let base: BTreeMap<String, Obs> =
            [("h1", 0.25), ("h2", 0.375), ("m1", 0.9), ("m2", 0.8)].iter().map(|(k, v)| (k.to_string(), obs(*v))).collect();
        let mut pp: BTreeMap<String, Obs> = BTreeMap::new();
        pp.insert("p:r:h1".into(), obs(0.25));
        pp.insert("p:r:m1".into(), obs(0.9 + 1e-12));
        pp.insert("p:p:h1".into(), obs(0.3125));
        pp.insert("p:p:h2".into(), obs(0.5));
        // m2's probe failed
        pp.insert("p:p:m2".into(), Obs::Failed("timeout".into()));
        let th = vec![("0.5".to_string(), 0.5), ("op_0.01".to_string(), 0.3)];
        let c = consistency(&r, &p, &base, &pp, &th).unwrap();
        let rep = &c["summaries"]["repeat"]["all"];
        assert_eq!(rep["n"], 2);
        assert_eq!(rep["bit_identical_involvement"], 1);
        assert_eq!(rep["equal_at_2_decimals"], 2);
        assert_eq!(c["repeats_all_bit_identical"], false);
        let per = &c["summaries"]["perturbed"]["all"];
        assert_eq!(per["n"], 2);
        assert!(per["mean_delta_involvement"].as_f64().unwrap() == 0.09375);
        assert!(per["max_abs_delta_involvement"].as_f64().unwrap() == 0.125);
        assert_eq!(per["abs_delta_at_least"]["0.05"], 2);
        assert_eq!(per["abs_delta_at_least"]["0.1"], 1);
        assert_eq!(per["decision_flips"]["0.5"]["flips"], 1);
        assert_eq!(per["decision_flips"]["0.5"]["newly_flagged"], 1);
        assert_eq!(per["decision_flips"]["op_0.01"]["base_flagged"], 1);
        assert_eq!(per["decision_flips"]["op_0.01"]["probe_flagged"], 2);
        assert_eq!(per["decision_flips"]["op_0.01"]["newly_flagged"], 1);
        assert_eq!(c["summaries"]["perturbed"]["operation:typo"]["n"], 1);
        assert_eq!(c["summaries"]["perturbed"]["origin:human_only"]["n"], 2);
        assert_eq!(c["accounting"]["perturbed"]["execution_failure"], 1);
        assert!(per["mean_delta_involvement_source_group_bootstrap_95"].is_array());
        // a prediction for an unlisted probe is an input error
        pp.insert("stray".into(), obs(0.1));
        assert!(consistency(&r, &p, &base, &pp, &th).is_err());
    }

    #[test]
    fn missing_observations_are_counted_not_scored() {
        let r = rows();
        let p = parse_probes(&manifest(), &r).unwrap();
        let base: BTreeMap<String, Obs> = [("h1".to_string(), obs(0.2))].into_iter().collect();
        let c = consistency(&r, &p, &base, &BTreeMap::new(), &[("0.5".into(), 0.5)]).unwrap();
        assert_eq!(c["accounting"]["repeat"]["probe_missing"], 1);
        assert_eq!(c["accounting"]["repeat"]["base_missing"], 1);
        assert_eq!(c["summaries"]["repeat"].as_object().unwrap().len(), 0);
        assert!(c["repeats_all_bit_identical"].is_null());
    }

    #[test]
    fn ood_rule_and_fpr() {
        let r = rows();
        let m = json!({"split": "train", "counts_domain_origin_generator": {
            "wikipedia": {"human_only": {"human": 10}, "model_only": {"qwen3.6": 3}},
            "spoken": {"model_only": {"qwen3.6": 2}}}});
        let mut rule = DomainRule::from_train_manifest(&m).unwrap();
        assert_eq!(rule.reason("wikipedia"), None);
        assert_eq!(rule.reason("spoken"), Some("domain_without_train_humans"));
        assert_eq!(rule.reason("newdomain"), Some("domain_absent_from_train"));
        rule.declared.insert("wikipedia".into());
        assert_eq!(rule.reason("wikipedia"), Some("declared"));
        rule.declared.clear();
        assert!(DomainRule::from_train_manifest(&json!({"split": "cal"})).is_err());
        let preds: BTreeMap<String, Obs> =
            [("h1", 0.1), ("h2", 0.7), ("h3", 0.6)].iter().map(|(k, v)| (k.to_string(), obs(*v))).collect();
        // h4 has no prediction: counted as flagged
        let v = ood_human_fpr(&r, &preds, &rule, &[("0.5".into(), 0.5), ("op_0.01".into(), 0.65)]);
        let o = &v["out_of_domain"];
        assert_eq!(o["humans"], 2);
        assert_eq!(o["fpr"]["0.5"]["count"], 2);
        assert_eq!(o["fpr"]["0.5"]["execution_failures_counted_as_flagged"], 1);
        assert_eq!(o["fpr"]["op_0.01"]["count"], 1);
        assert_eq!(v["in_domain"]["humans"], 2);
        assert_eq!(v["in_domain"]["fpr"]["0.5"]["count"], 1);
        assert_eq!(v["by_population"]["sbcsae_spoken_conversation"]["humans"], 1);
        assert_eq!(v["domain_reasons"]["newdomain"], "domain_absent_from_train");
        let u = o["fpr"]["0.5"]["clopper_pearson_upper_one_sided_95"].as_f64().unwrap();
        assert!((u - 1.0).abs() < 1e-12);
    }

    #[test]
    fn thresholds_from_a_real_evaluation() {
        use crate::comp_eval::{Missing, Options, SeedInput, SystemInput, evaluate};
        let r = rows();
        let preds: BTreeMap<String, Obs> = r.iter().map(|x| (x.id.clone(), obs(if x.is_human() { 0.25 } else { 0.75 }))).collect();
        let sys = vec![SystemInput {
            name: "A".into(),
            seeds: vec![SeedInput { seed: 17, predictions: preds.clone(), operating_points: None }],
            subsample: None,
            missing: Missing::Worst,
        }];
        let e = evaluate(&r, &sys, &[], &Options::default()).unwrap();
        let th = seed_thresholds(&e, "A", 17).unwrap();
        assert_eq!(th.len(), 3, "{th:?}");
        assert!(th.iter().all(|(_, t)| *t > 0.25 && *t <= 0.75 || t.is_infinite() || *t == 0.5), "{th:?}");
        let p = parse_probes(&manifest(), &r).unwrap();
        let pp: BTreeMap<String, Obs> = p.iter().map(|x| (x.probe_id.clone(), preds[&x.base_id].clone())).collect();
        let c = consistency(&r, &p, &preds, &pp, &th).unwrap();
        assert_eq!(c["repeats_all_bit_identical"], true);
        assert_eq!(c["summaries"]["perturbed"]["all"]["mean_abs_delta_involvement"], 0.0);
        let md = markdown(&json!({"consistency": {"A:17": c}}));
        assert!(md.contains("| A:17 | repeat | all | 2 |"));
    }

    #[test]
    fn thresholds_read_exact_bits() {
        let t: f64 = 0.123456789;
        let e = json!({"systems": {"A": {"seeds": {"17": {"operating_points": {
            "0.01": {"threshold": {"value": t, "bits": format!("0x{:016x}", t.to_bits())}},
            "0.001": {"threshold": {"value": "inf", "bits": format!("0x{:016x}", f64::INFINITY.to_bits())}}}}}}}});
        let v = seed_thresholds(&e, "A", 17).unwrap();
        assert_eq!(v[0], ("0.5".to_string(), 0.5));
        assert!(v.contains(&("op_0.01".to_string(), t)));
        assert!(v.iter().any(|(l, x)| l == "op_0.001" && x.is_infinite()));
        let md = markdown(&json!({"consistency": {}, "out_of_domain_humans": {}}));
        assert!(md.is_empty());
    }
}
