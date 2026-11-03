//! Report for the membership twins: the released A (seed 17, the frozen
//! comparator R0) on every arm, the host-and-month matched gaps, and the
//! lookup-canary statistic with its power.
//!
//! Canary for artifact i with twins as the non-member arm:
//! `T_i = sum_s w_s (mean_frame_s D_i - mean_twin_s D_i)`, where
//! `D_i = u_i - u_0`, `u = 1 - (a - y)^2`, strata `s` are host x URL month
//! and `w_s` is the stratum's share of twins. A flag is
//! `T_i - tau_hat > 3.09 SE_i` with SE from a host bootstrap.
//!
//! The `report` step scores only R0 on this set, so for R0 itself `D = 0`. The
//! other honest models enter through Tier C Test: each Tier C human gives a
//! logit shift `logit(a_m) - logit(a_0)` of model m against R0, and a CC
//! document draws shifts from Tier C humans in its own R0 score bin. That
//! keeps each honest model's R0-conditional behaviour, so the honest T it
//! yields measures how far the twin and frame arms differ for R0 (the
//! composition part of tau), not any source-specific behaviour of model m.
//!
//! The `tau` step measures tau directly from honest models scored on the
//! twin set itself, one `T_m` per model against R0.
use crate::{
    measure::{Rng, percentile},
    store,
    twins::Doc,
};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    fs,
    io::{BufRead, BufReader},
    path::Path,
};

pub struct Args<'a> {
    pub root: &'a Path,
    pub a_pred: &'a Path,
    pub tierc_rows: &'a Path,
    pub tierc_pred: &'a Path,
    pub threshold: f64,
    pub sims: usize,
    pub boot: usize,
    /// Optional corpus-v1 site roles (registered domain, role, rows), the pilot's host-closure cache.
    pub site_roles: Option<&'a Path>,
    /// Apply `twins::copy_risk_host` (mirror, syndication and archive hosts).
    pub copy_risk_rule: bool,
}

pub const PANEL_CLOSE: &[&str] = &["Faux-29", "Faux-41"];
pub const PANEL_DIFFERENT: &[&str] = &["E0-17", "E0-29", "E0-41", "S3A-17", "S3A-29", "S3A-41", "V6-17"];
const Z_ALPHA: f64 = 3.090232306167813; // one-sided 0.001
const Z_80: f64 = 0.8416212335729143;
const BINS: usize = 20;
const EPS: f64 = 1e-6;

fn logit(p: f64) -> f64 {
    let p = p.clamp(EPS, 1.0 - EPS);
    (p / (1.0 - p)).ln()
}
fn sigmoid(x: f64) -> f64 {
    1.0 / (1.0 + (-x).exp())
}
fn mean(v: &[f64]) -> f64 {
    if v.is_empty() { f64::NAN } else { v.iter().sum::<f64>() / v.len() as f64 }
}
fn sd(v: &[f64]) -> f64 {
    if v.len() < 2 {
        return f64::NAN;
    }
    let m = mean(v);
    (v.iter().map(|x| (x - m).powi(2)).sum::<f64>() / (v.len() - 1) as f64).sqrt()
}
/// Standard normal CDF (Abramowitz-Stegun 7.1.26 via erf).
pub fn phi(x: f64) -> f64 {
    let t = 1.0 / (1.0 + 0.3275911 * x.abs() / std::f64::consts::SQRT_2);
    let y = 1.0 - (((((1.061405429 * t - 1.453152027) * t) + 1.421413741) * t - 0.284496736) * t + 0.254829592) * t * (-(x * x) / 2.0).exp();
    if x >= 0.0 { 0.5 * (1.0 + y) } else { 0.5 * (1.0 - y) }
}

/// A matched design: strata holding at least one twin and one frame doc.
struct Design {
    /// (stratum weight, twin doc indices, frame doc indices)
    strata: Vec<(f64, Vec<usize>, Vec<usize>)>,
    /// host -> stratum indices, for the host bootstrap
    hosts: Vec<Vec<usize>>,
}

impl Design {
    fn new(docs: &[Doc], a_arm: &str, b_arm: &str) -> Design {
        let mut by: BTreeMap<&str, (Vec<usize>, Vec<usize>)> = BTreeMap::new();
        for (i, d) in docs.iter().enumerate() {
            if d.excluded.is_some() {
                continue;
            }
            if d.arm == a_arm {
                by.entry(&d.stratum).or_default().0.push(i);
            } else if d.arm == b_arm {
                by.entry(&d.stratum).or_default().1.push(i);
            }
        }
        let kept: Vec<(&str, Vec<usize>, Vec<usize>)> = by.into_iter().filter(|(_, (t, f))| !t.is_empty() && !f.is_empty()).map(|(s, (t, f))| (s, t, f)).collect();
        let n: usize = kept.iter().map(|x| x.1.len()).sum();
        let mut hmap: BTreeMap<String, Vec<usize>> = BTreeMap::new();
        let mut strata = Vec::new();
        for (k, (s, t, f)) in kept.into_iter().enumerate() {
            hmap.entry(s.split('|').next().unwrap_or("").to_owned()).or_default().push(k);
            strata.push((t.len() as f64 / n as f64, t, f));
        }
        Design {
            strata,
            hosts: hmap.into_values().collect(),
        }
    }
    fn n_a(&self) -> usize {
        self.strata.iter().map(|s| s.1.len()).sum()
    }
    fn n_b(&self) -> usize {
        self.strata.iter().map(|s| s.2.len()).sum()
    }
    /// sum_s w_s (mean_b x - mean_a x) over the strata in `pick` (weights renormalised).
    fn diff_b_minus_a(&self, x: &[f64], pick: &[usize]) -> f64 {
        let (mut num, mut den) = (0.0, 0.0);
        for &k in pick {
            let (w, t, f) = &self.strata[k];
            let mt = t.iter().map(|&i| x[i]).sum::<f64>() / t.len() as f64;
            let mf = f.iter().map(|&i| x[i]).sum::<f64>() / f.len() as f64;
            num += w * (mf - mt);
            den += w;
        }
        num / den
    }
    fn all(&self) -> Vec<usize> {
        (0..self.strata.len()).collect()
    }
    fn boot(&self, x: &[f64], draws: usize, seed: u64) -> (f64, f64, f64) {
        let mut rng = Rng(seed);
        let mut v = Vec::with_capacity(draws);
        for _ in 0..draws {
            let mut pick = Vec::new();
            for _ in 0..self.hosts.len() {
                pick.extend_from_slice(&self.hosts[rng.below(self.hosts.len())]);
            }
            v.push(self.diff_b_minus_a(x, &pick));
        }
        let s = sd(&v);
        (percentile(&mut v.clone(), 0.025), percentile(&mut v, 0.975), s)
    }
    /// Independent-documents SE of the stratified difference.
    fn iid_se(&self, x: &[f64]) -> f64 {
        let pooled = |ix: &[usize]| -> f64 {
            let v: Vec<f64> = ix.iter().map(|&i| x[i]).collect();
            let s = sd(&v);
            if s.is_finite() { s * s } else { 0.0 }
        };
        let all_a: Vec<usize> = self.strata.iter().flat_map(|s| s.1.clone()).collect();
        let all_b: Vec<usize> = self.strata.iter().flat_map(|s| s.2.clone()).collect();
        let (va, vb) = (pooled(&all_a), pooled(&all_b));
        self.strata.iter().map(|(w, t, f)| w * w * (va / t.len() as f64 + vb / f.len() as f64)).sum::<f64>().sqrt()
    }
    /// Within-stratum P(x_b > x_a) + 0.5 P(tie), pooled over within-stratum pairs.
    fn auc_b_over_a(&self, x: &[f64]) -> f64 {
        let (mut num, mut den) = (0.0, 0.0);
        for (_, t, f) in &self.strata {
            for &i in f {
                for &j in t {
                    num += if x[i] > x[j] { 1.0 } else if x[i] == x[j] { 0.5 } else { 0.0 };
                    den += 1.0;
                }
            }
        }
        num / den
    }
}

fn load_pred(dir: &Path, name: &str) -> Result<HashMap<String, f64>> {
    let mut m = HashMap::new();
    let r = BufReader::new(fs::File::open(dir.join(format!("{name}.jsonl"))).with_context(|| format!("{name} predictions"))?);
    for l in r.lines() {
        let v: Value = serde_json::from_str(&l?)?;
        m.insert(v["id"].as_str().unwrap_or("").to_owned(), 1.0 - v["p_human"].as_f64().unwrap_or(f64::NAN));
    }
    Ok(m)
}

struct Panel {
    name: String,
    close: bool,
    /// per bin: logit shifts of model m against R0 on Tier C humans
    shifts: Vec<Vec<f64>>,
    tierc_tau_h: f64,
    tierc_sigma_h: f64,
}

fn bin_of(edges: &[f64], a: f64) -> usize {
    edges.partition_point(|e| *e <= logit(a)).min(edges.len())
}

/// The twin set's documents with the released A (R0) predictions joined by id and text
/// SHA-256, copy-risk hosts (if asked) and unscored documents left out of the matched analysis.
pub(crate) struct Scored {
    pub docs: Vec<Doc>,
    pub a0: Vec<f64>,
    pub failures: usize,
    pub missing: usize,
    pub copy_risk: BTreeMap<String, &'static str>,
}

/// One model's involvement per document of `docs` (NaN where absent or not `ok`); every
/// prediction must belong to a document and carry its scored text's SHA-256.
fn involvement_by_doc(docs: &[Doc], pred: &Path) -> Result<(Vec<f64>, usize)> {
    let idx: HashMap<&str, usize> = docs.iter().enumerate().map(|(i, d)| (d.id.as_str(), i)).collect();
    let mut a = vec![f64::NAN; docs.len()];
    let mut seen = vec![false; docs.len()];
    let mut failures = 0;
    for l in BufReader::new(fs::File::open(pred).with_context(|| format!("{}", pred.display()))?).lines() {
        let v: Value = serde_json::from_str(&l?)?;
        let id = v["id"].as_str().context("pred id")?;
        let i = *idx.get(id).with_context(|| format!("{id}: prediction outside the docs"))?;
        ensure!(!seen[i], "{id}: two predictions");
        seen[i] = true;
        ensure!(
            v["text_sha256"].as_str() == Some(crate::measure::sha256_hex(docs[i].scored_text().as_bytes()).as_str()),
            "{id}: scored text differs"
        );
        if v["status"].as_str() == Some("ok") {
            a[i] = v["prediction"]["attribution"]["ai"]["involvement_probability"].as_f64().context("involvement")?;
        } else {
            failures += 1;
        }
    }
    Ok((a, failures))
}

fn load_scored(root: &Path, a_pred: &Path, copy_risk_rule: bool) -> Result<Scored> {
    let mut docs: Vec<Doc> = store::rows(&root.join("extract").join("docs.jsonl.zst"))?;
    let (a0, failures) = involvement_by_doc(&docs, a_pred)?;
    let missing = a0.iter().filter(|x| x.is_nan()).count();
    // Hosts whose twins are probably copies held by the frame elsewhere leave the matched analysis.
    let mut copy_risk: BTreeMap<String, &'static str> = BTreeMap::new();
    if copy_risk_rule {
        for d in docs.iter_mut() {
            if let Some(r) = crate::twins::copy_risk_host(&d.host, &d.registered_domain) {
                copy_risk.insert(d.host.clone(), r);
                if d.excluded.is_none() {
                    d.excluded = Some(format!("copy_risk_host:{r}"));
                }
            }
        }
    }
    for (i, d) in docs.iter_mut().enumerate() {
        if a0[i].is_nan() && d.excluded.is_none() {
            d.excluded = Some("no_a_prediction".into());
        }
    }
    Ok(Scored { docs, a0, failures, missing, copy_risk })
}

/// Power of the canary rule `T - 1.96 tau > 3.09 SE` for one design:
/// `SE = sigma sqrt(de) sqrt(1/n_twin + 1/n_frame)`; an honest model's T is spread over
/// `sqrt(SE^2 + tau^2)`; c80 is the frame coverage flagged with 80% power at effect `ell`.
#[allow(clippy::too_many_arguments)]
fn power_row(stat: &str, panel: &str, sigma: f64, tau: f64, tau_src: &str, ell: f64, nt: f64, nf: f64, de: f64) -> Value {
    let se = sigma * de.sqrt() * (1.0 / nt + 1.0 / nf).sqrt();
    let thr = 1.96 * tau + Z_ALPHA * se;
    let tot = (se * se + tau * tau).sqrt();
    json!({"statistic": stat, "panel": panel, "sigma_doc": sigma, "tau": tau, "tau_source": tau_src, "ell_full_coverage": ell,
           "n_twin": nt, "n_frame": nf, "design_factor": de, "se": se, "threshold": thr,
           "honest_flag_rate_per_audit": 1.0 - phi(thr / tot), "c80": (thr + Z_80 * tot) / ell})
}

pub fn report(a: &Args) -> Result<Value> {
    let root = a.root;
    let Scored { docs, a0, failures, missing, copy_risk } = load_scored(root, a.a_pred, a.copy_risk_rule)?;
    let loss: Vec<f64> = a0.iter().map(|x| x * x).collect();
    let flag: Vec<f64> = a0.iter().map(|x| (*x >= a.threshold) as u8 as f64).collect();
    let lg: Vec<f64> = a0.iter().map(|x| logit(*x)).collect();

    // ---------------- arms
    let mut arms = serde_json::Map::new();
    for arm in ["twin", "frame", "frame_post"] {
        let ix: Vec<usize> = (0..docs.len()).filter(|&i| docs[i].arm == arm && docs[i].excluded.is_none()).collect();
        let mut dom: BTreeMap<&str, usize> = BTreeMap::new();
        let mut yr: BTreeMap<&str, usize> = BTreeMap::new();
        let mut src: BTreeMap<String, usize> = BTreeMap::new();
        for &i in &ix {
            *dom.entry(&docs[i].domain).or_default() += 1;
            *yr.entry(&docs[i].url_month[..4]).or_default() += 1;
            *src.entry(docs[i].page_published_source.clone().unwrap_or_else(|| "none (URL month only)".into())).or_default() += 1;
        }
        let pick = |v: &[f64]| ix.iter().map(|&i| v[i]).collect::<Vec<f64>>();
        let fl = pick(&flag);
        let wl = crate::measure::wilson(fl.iter().sum::<f64>() as usize, ix.len());
        arms.insert(arm.into(), json!({"docs": ix.len(),
            "hosts": ix.iter().map(|&i| docs[i].host.as_str()).collect::<BTreeSet<_>>().len(),
            "strata": ix.iter().map(|&i| docs[i].stratum.as_str()).collect::<BTreeSet<_>>().len(),
            "mean_involvement": mean(&pick(&a0)), "mean_loss_a2": mean(&pick(&loss)),
            "flagged": fl.iter().sum::<f64>(), "flag_rate": mean(&fl),
            "flag_rate_wilson95": [wl.0, wl.1],
            "mean_words": mean(&ix.iter().map(|&i| docs[i].words as f64).collect::<Vec<_>>()),
            "domains": dom, "url_years": yr, "page_date_source": src}));
    }
    let mut excl: BTreeMap<String, usize> = BTreeMap::new();
    for d in &docs {
        if let Some(r) = &d.excluded {
            *excl.entry(format!("{}:{r}", d.arm)).or_default() += 1;
        }
    }

    // ---------------- matched gaps for R0
    let tf = Design::new(&docs, "twin", "frame");
    ensure!(!tf.strata.is_empty(), "no matched strata");
    let gap = |d: &Design, x: &[f64], seed: u64| -> Value {
        let est = d.diff_b_minus_a(x, &d.all());
        let (lo, hi, se) = d.boot(x, a.boot.max(1000), seed);
        let iid = d.iid_se(x);
        json!({"frame_minus_twin": est, "ci95_host_bootstrap": [lo, hi], "se_host_bootstrap": se, "se_independent": iid, "design_effect": (se / iid).powi(2)})
    };
    let tp = Design::new(&docs, "twin", "frame_post");
    let fp = Design::new(&docs, "frame_post", "frame");
    // paired same-URL frame vs frame_post
    let mut pair_d = Vec::new();
    let mut pair_same_text = 0usize;
    let by_id: HashMap<&str, usize> = docs.iter().enumerate().map(|(i, d)| (d.id.as_str(), i)).collect();
    for (i, d) in docs.iter().enumerate() {
        if d.arm == "frame_post" && d.excluded.is_none()
            && let Some(&j) = d.pair.as_deref().and_then(|p| by_id.get(p))
            && docs[j].excluded.is_none() && a0[j].is_finite() && a0[i].is_finite()
        {
            pair_d.push(loss[i] - loss[j]);
            pair_same_text += (docs[i].folded_sha256 == docs[j].folded_sha256) as usize;
        }
    }
    let matched = json!({
        "twin_vs_frame": {"strata": tf.strata.len(), "hosts": tf.hosts.len(), "twins": tf.n_a(), "frame": tf.n_b(),
            "loss_a2": gap(&tf, &loss, 11), "flag": gap(&tf, &flag, 12), "involvement": gap(&tf, &a0, 13), "logit_involvement": gap(&tf, &lg, 14),
            "auc_frame_over_twin_involvement": tf.auc_b_over_a(&a0)},
        "twin_vs_frame_post": {"strata": tp.strata.len(), "hosts": tp.hosts.len(), "twins": tp.n_a(), "frame_post": tp.n_b(),
            "loss_a2": if tp.strata.is_empty() { Value::Null } else { gap(&tp, &loss, 21) },
            "flag": if tp.strata.is_empty() { Value::Null } else { gap(&tp, &flag, 22) }},
        "frame_post_vs_frame_same_strata": {"strata": fp.strata.len(), "loss_a2": if fp.strata.is_empty() { Value::Null } else { gap(&fp, &loss, 31) }},
        "same_url_pairs": {"pairs": pair_d.len(), "identical_folded_excerpt": pair_same_text, "mean_loss_post_minus_pre": mean(&pair_d), "sd": sd(&pair_d)},
    });

    // ---------------- Tier C honest panel
    let mut human_ids: Vec<(String, String)> = Vec::new();
    for l in BufReader::new(fs::File::open(a.tierc_rows)?).lines() {
        let v: Value = serde_json::from_str(&l?)?;
        // The main Tier C human set (3,286 documents): diagnostic cohorts are left out.
        if v["origin"].as_str() == Some("human_only") && v["cohort_id"].as_str().is_some_and(|c| c.starts_with("human:")) {
            human_ids.push((v["id"].as_str().unwrap_or("").to_owned(), v["domain"].as_str().unwrap_or("").to_owned()));
        }
    }
    let r0 = load_pred(a.tierc_pred, "Faux-17")?;
    let tc_a0: Vec<f64> = human_ids.iter().map(|(id, _)| r0.get(id).copied().unwrap_or(f64::NAN)).collect();
    ensure!(tc_a0.iter().all(|x| x.is_finite()), "Tier C R0 predictions missing");
    let mut lgs: Vec<f64> = tc_a0.iter().map(|x| logit(*x)).collect();
    lgs.sort_by(|x, y| x.total_cmp(y));
    // Quantile edges of R0's Tier C human logits; calibrated scores tie, so equal edges merge.
    let mut edges: Vec<f64> = (1..BINS).map(|k| lgs[k * lgs.len() / BINS]).collect();
    edges.dedup();
    let nb = edges.len() + 1;
    let tc_bin: Vec<usize> = tc_a0.iter().map(|x| bin_of(&edges, *x)).collect();
    let tc_lh = mean(&tc_a0.iter().map(|x| x * x).collect::<Vec<_>>());
    let mut panel: Vec<Panel> = Vec::new();
    for name in PANEL_CLOSE.iter().chain(PANEL_DIFFERENT.iter()) {
        let pm = load_pred(a.tierc_pred, name)?;
        let mut shifts = vec![Vec::new(); nb];
        let mut d_h = Vec::new();
        let mut by_dom: BTreeMap<&str, Vec<f64>> = BTreeMap::new();
        for (k, (id, dom)) in human_ids.iter().enumerate() {
            let Some(&am) = pm.get(id) else { continue };
            shifts[tc_bin[k]].push(logit(am) - logit(tc_a0[k]));
            let dd = tc_a0[k].powi(2) - am * am;
            d_h.push(dd);
            by_dom.entry(dom).or_default().push(dd);
        }
        let dm: Vec<f64> = by_dom.values().map(|v| mean(v)).collect();
        let noise = mean(&by_dom.values().map(|v| sd(v).powi(2) / v.len() as f64).collect::<Vec<_>>());
        let tau = (sd(&dm).powi(2) - noise).max(0.0).sqrt();
        panel.push(Panel {
            name: name.to_string(),
            close: PANEL_CLOSE.contains(name),
            shifts,
            tierc_tau_h: tau,
            tierc_sigma_h: sd(&d_h),
        });
    }
    // A bin without Tier C humans borrows the nearest non-empty bin.
    for p in panel.iter_mut() {
        let filled: Vec<usize> = (0..nb).filter(|&b| !p.shifts[b].is_empty()).collect();
        ensure!(!filled.is_empty(), "{}: no Tier C shifts", p.name);
        for b in 0..nb {
            if p.shifts[b].is_empty() {
                let near = *filled.iter().min_by_key(|&&f| (f as i64 - b as i64).abs()).unwrap();
                p.shifts[b] = p.shifts[near].clone();
            }
        }
    }
    let cc_bin: Vec<usize> = a0.iter().map(|x| if x.is_finite() { bin_of(&edges, *x) } else { 0 }).collect();
    let mut cc_bins_by_arm: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
    for (i, d) in docs.iter().enumerate() {
        if d.excluded.is_none() {
            let e = cc_bins_by_arm.entry(d.arm.as_str()).or_insert_with(|| vec![0; nb]);
            e[cc_bin[i]] += 1;
        }
    }
    // Composition part of honest T: expected D_m per doc under the bin's shifts.
    let mut comp = serde_json::Map::new();
    let mut t_comp: Vec<(bool, f64)> = Vec::new();
    let mut t_comp_se: Vec<(bool, f64)> = Vec::new();
    let mut sigma_sim: Vec<(bool, f64)> = Vec::new();
    for p in &panel {
        let ed: Vec<f64> = (0..docs.len())
            .map(|i| {
                if !a0[i].is_finite() {
                    return 0.0;
                }
                let s = &p.shifts[cc_bin[i]];
                s.iter().map(|d| a0[i].powi(2) - sigmoid(lg[i] + d).powi(2)).sum::<f64>() / s.len() as f64
            })
            .collect();
        let t = tf.diff_b_minus_a(&ed, &tf.all());
        let (lo, hi, se) = tf.boot(&ed, 1000, 41);
        // one draw per doc to get the per-document SD of D_m on this set
        let mut rng = Rng(0x5157 ^ p.name.len() as u64);
        let mut sds = Vec::new();
        for _ in 0..20 {
            let v: Vec<f64> = (0..docs.len())
                .filter(|&i| docs[i].excluded.is_none() && a0[i].is_finite() && (docs[i].arm == "twin" || docs[i].arm == "frame"))
                .map(|i| {
                    let s = &p.shifts[cc_bin[i]];
                    a0[i].powi(2) - sigmoid(lg[i] + s[rng.below(s.len())]).powi(2)
                })
                .collect();
            sds.push(sd(&v));
        }
        let sg = mean(&sds);
        t_comp.push((p.close, t));
        t_comp_se.push((p.close, se));
        sigma_sim.push((p.close, sg));
        comp.insert(p.name.clone(), json!({"panel": if p.close { "close" } else { "different" },
            "T_composition": t, "T_composition_ci95_host_bootstrap": [lo, hi], "T_composition_se": se,
            "mean_expected_D_twin_frame": mean(&(0..docs.len()).filter(|&i| docs[i].excluded.is_none() && (docs[i].arm == "twin" || docs[i].arm == "frame")).map(|i| ed[i]).collect::<Vec<_>>()),
            "sigma_doc_D_on_this_set": sg, "tierc_sigma_doc_D_humans": p.tierc_sigma_h, "tierc_tau_between_domains_humans": p.tierc_tau_h}));
    }
    let tau_of = |close: bool| -> (f64, f64) {
        let mut v: Vec<f64> = t_comp.iter().filter(|x| x.0 == close).map(|x| x.1.abs()).collect();
        let rms = (v.iter().map(|x| x * x).sum::<f64>() / v.len() as f64).sqrt();
        (rms, percentile(&mut v, 0.95))
    };
    let (tau_c_close, tau95_close) = tau_of(true);
    let (tau_c_diff, tau95_diff) = tau_of(false);
    // The realised composition gap of a finite twin set includes its own sampling noise (which
    // shrinks with more twins); the systematic part is rms(T) with the bootstrap variance removed.
    let tau_sys = |close: bool| -> f64 {
        let t2 = mean(&t_comp.iter().filter(|x| x.0 == close).map(|x| x.1 * x.1).collect::<Vec<_>>());
        let s2 = mean(&t_comp_se.iter().filter(|x| x.0 == close).map(|x| x.1 * x.1).collect::<Vec<_>>());
        (t2 - s2).max(0.0).sqrt()
    };
    let (tau_s_close, tau_s_diff) = (tau_sys(true), tau_sys(false));
    let mut all_abs: Vec<f64> = t_comp.iter().map(|x| x.1.abs()).collect();
    let tau_hat_panel = percentile(&mut all_abs, 0.95);
    let sig_close = mean(&sigma_sim.iter().filter(|x| x.0).map(|x| x.1).collect::<Vec<_>>());
    let sig_diff = mean(&sigma_sim.iter().filter(|x| !x.0).map(|x| x.1).collect::<Vec<_>>());
    let tierc_tau_close = mean(&panel.iter().filter(|p| p.close).map(|p| p.tierc_tau_h).collect::<Vec<_>>());
    let tierc_tau_diff = mean(&panel.iter().filter(|p| !p.close).map(|p| p.tierc_tau_h).collect::<Vec<_>>());

    // ---------------- effect, design effect and power
    let frame_ix: Vec<usize> = tf.strata.iter().flat_map(|s| s.2.clone()).collect();
    // Full coverage of the frame arm's human documents: the covered document's loss goes to ~0,
    // weighted as in T (stratum weights over frame means).
    let ell_h = {
        let mut x = vec![0.0; docs.len()];
        for &i in &frame_ix {
            x[i] = loss[i];
        }
        tf.diff_b_minus_a(&x, &tf.all())
    };
    let (n_t, n_f) = (tf.n_a() as f64, tf.n_b() as f64);
    // Design factor of this set: the host-bootstrap SE of R0's matched loss gap over the
    // SE of a simple two-sample difference with the pooled per-document SD. It folds in the
    // unequal stratum weights and the clustering, and is carried to larger sets of the same design.
    let (deff, design_factor) = {
        let (_, _, se) = tf.boot(&loss, 1000, 51);
        let pooled: Vec<f64> = tf.strata.iter().flat_map(|s| s.1.iter().chain(s.2.iter()).map(|&i| loss[i]).collect::<Vec<_>>()).collect();
        ((se / tf.iid_se(&loss)).powi(2), (se / (sd(&pooled) * (1.0 / n_t + 1.0 / n_f).sqrt())).powi(2))
    };
    let mut power = Vec::new();
    let sizes = [(n_t, n_f, "measured"), (2000.0, 2000.0, "2k"), (20000.0, 20000.0, "20k")];
    for (nt, nf, _) in sizes {
        for (pn, sg, tc, ts, ttc) in [("close", sig_close, tau_c_close, tau_s_close, tierc_tau_close), ("different", sig_diff, tau_c_diff, tau_s_diff, tierc_tau_diff)] {
            for (tau, src) in [(0.0, "zero (perfect match)"), (ts, "composition, systematic part (noise removed)"), (tc, "composition, as realised on this twin set (rms over the panel)"),
                               (0.0025, "sensitivity 0.0025"), (0.005, "sensitivity 0.005"), (ttc, "Tier C between-domain, humans (unmatched sources)")] {
                power.push(power_row("human_document", pn, sg, tau, src, ell_h, nt, nf, design_factor));
            }
        }
    }
    // Plan family statistic (human document and its mixed children averaged): children cannot be
    // generated here, so the child terms come from the lookup measurement on Tier C.
    let fam = |pn: &str| -> (f64, f64) {
        if pn == "close" { (0.0519, 0.0048) } else { (0.1282, 0.019) }
    };
    let child_gain_b = 2.0 * 0.0652 - 2.0 * 0.0353; // lookup-v1 canary-none: family ell B minus A, doubled
    for (nt, nf, _) in sizes {
        for (pn, tc, ts) in [("close", tau_c_close, tau_s_close), ("different", tau_c_diff, tau_s_diff)] {
            let (sf, tauf) = fam(pn);
            for (strat, ell) in [("family_A", ell_h / 2.0), ("family_B", (ell_h + child_gain_b) / 2.0)] {
                for (tau, src) in [(0.0, "zero"), (ts, "composition, systematic part (human part, assumed for children too)"), (tc, "composition as realised (human part, assumed for children too)"),
                                   (0.0025, "sensitivity 0.0025"), (0.005, "sensitivity 0.005"), (tauf, "Tier C between-domain, families (lookup-v1)")] {
                    power.push(power_row(strat, pn, sf, tau, src, ell, nt, nf, design_factor));
                }
            }
        }
    }

    // ---------------- simulation of the canary on this set (human-document statistic)
    let use_ix: Vec<usize> = tf.strata.iter().flat_map(|s| s.1.iter().chain(s.2.iter()).copied().collect::<Vec<_>>()).collect();
    let is_frame: Vec<bool> = (0..docs.len()).map(|i| docs[i].arm == "frame").collect();
    let coverages = [0.0, 0.05, 0.1, 0.2, 0.3, 0.5, 1.0];
    let mut sim_rows = Vec::new();
    let rules = [("tau_hat = 95th percentile of |T| over the honest panel (composition)", tau_hat_panel),
                 ("tau_hat = 1.96 x Tier C between-domain tau, close panel", 1.96 * tierc_tau_close),
                 ("tau_hat = 1.96 x Tier C between-domain tau, different panel", 1.96 * tierc_tau_diff),
                 ("tau_hat = 0", 0.0)];
    for pclose in [true, false] {
        let models: Vec<&Panel> = panel.iter().filter(|p| p.close == pclose).collect();
        for &c in &coverages {
            let mut rng = Rng(0xC0FFEE ^ (c * 1000.0) as u64 ^ ((pclose as u64) << 40));
            let mut ts = Vec::new();
            let mut ses = Vec::new();
            for r in 0..a.sims {
                let m = models[r % models.len()];
                let mut dvec = vec![0.0; docs.len()];
                for &i in &use_ix {
                    let s = &m.shifts[cc_bin[i]];
                    let mut am = sigmoid(lg[i] + s[rng.below(s.len())]);
                    if is_frame[i] && (rng.next_u64() as f64 / u64::MAX as f64) < c {
                        am *= 1e-3;
                    }
                    dvec[i] = loss[i] - am * am;
                }
                ts.push(tf.diff_b_minus_a(&dvec, &tf.all()));
                ses.push(tf.boot(&dvec, a.boot, rng.next_u64()).2);
            }
            for (rule, tau_hat) in rules {
                let flags = ts.iter().zip(&ses).filter(|(t, se)| **t - tau_hat > Z_ALPHA * **se).count();
                sim_rows.push(json!({"tau_rule": rule, "tau_hat": tau_hat, "panel": if pclose { "close" } else { "different" }, "coverage": c,
                    "sims": a.sims, "flag_rate": flags as f64 / a.sims as f64, "mean_T": mean(&ts), "sd_T": sd(&ts), "mean_se": mean(&ses)}));
            }
        }
    }

    // ---------------- hosts that the released model's training data covers
    let mut roles_section = Value::Null;
    if let Some(p) = a.site_roles {
        let raw = zstd::decode_all(&fs::read(p)?[..])?;
        let mut roles: HashMap<String, BTreeSet<String>> = HashMap::new();
        for l in String::from_utf8_lossy(&raw).lines().skip(1) {
            let f: Vec<&str> = l.split('\t').collect();
            if f.len() >= 3 && !f[1].is_empty() {
                roles.entry(f[0].to_owned()).or_default().insert(f[1].to_owned());
            }
        }
        let cls = |d: &Doc| -> &'static str {
            match roles.get(&d.registered_domain) {
                None => "no_corpus_v1_rows",
                Some(r) if r.iter().any(|x| x.starts_with("train")) => "corpus_v1_train_rows",
                Some(_) => "corpus_v1_other_rows",
            }
        };
        let mut by_arm: BTreeMap<String, BTreeMap<&'static str, usize>> = BTreeMap::new();
        for d in docs.iter().filter(|d| d.excluded.is_none()) {
            *by_arm.entry(d.arm.clone()).or_default().entry(cls(d)).or_default() += 1;
        }
        let mut sub = serde_json::Map::new();
        for c in ["no_corpus_v1_rows", "corpus_v1_train_rows", "corpus_v1_other_rows"] {
            let mut dd = docs.clone();
            for d in dd.iter_mut() {
                if cls(d) != c && d.excluded.is_none() {
                    d.excluded = Some("subset".into());
                }
            }
            let ds = Design::new(&dd, "twin", "frame");
            if ds.strata.len() >= 5 {
                let est = ds.diff_b_minus_a(&loss, &ds.all());
                let (lo, hi, se) = ds.boot(&loss, 1000, 61);
                sub.insert(c.into(), json!({"strata": ds.strata.len(), "hosts": ds.hosts.len(), "twins": ds.n_a(), "frame": ds.n_b(),
                    "loss_frame_minus_twin": est, "ci95": [lo, hi], "se": se, "flag_frame_minus_twin": ds.diff_b_minus_a(&flag, &ds.all())}));
            }
        }
        roles_section = json!({"source": p, "docs_by_arm": by_arm, "matched_gap_by_host_class": sub});
    }

    let summary = json!({
        "schema": "slop_ninja_cc_twins_report_v1",
        "host_corpus_v1_roles": roles_section,
        "copy_risk_hosts_excluded": copy_risk,
        "a": {"model": "released A seed 17 (Faux / E1aux), calibrated Rust FP64 runtime full_a-2314d433 on CPU", "threshold": a.threshold,
              "predictions": a.a_pred, "failures": failures, "missing": missing},
        "arms": arms, "excluded": excl, "matched": matched,
        "tier_c": {"humans": human_ids.len(), "R0_mean_loss_humans": tc_lh, "bins": nb, "bin_edges_logit": edges,
                   "cc_docs_per_bin_by_arm": cc_bins_by_arm, "panel": comp,
                   "tau_composition_rms": {"close": tau_c_close, "different": tau_c_diff},
                   "tau_composition_systematic": {"close": tau_s_close, "different": tau_s_diff},
                   "tau_composition_p95": {"close": tau95_close, "different": tau95_diff, "all": tau_hat_panel},
                   "tierc_tau_between_domains_humans": {"close": tierc_tau_close, "different": tierc_tau_diff},
                   "sigma_doc_D_on_this_set": {"close": sig_close, "different": sig_diff}},
        "effect": {"ell_human_full_coverage": ell_h, "child_gain_B_from_lookup_v1": child_gain_b,
                   "family_sigma_tau_from_lookup_v1": {"close": [0.0519, 0.0048], "different": [0.1282, 0.019]}},
        "design_effect_from_R0_loss": deff,
        "design_factor_for_power": design_factor,
        "power": power,
        "simulation": {"rule": "honest model m: per document a_m = sigmoid(logit(a_0) + shift), shift drawn from Tier C humans in the document's R0 bin; lookup attacker: m with a_m x 1e-3 on a random fraction c of frame documents; T as specified, SE from a host bootstrap; flag when T - tau_hat > 3.09 SE", "boot": a.boot, "rows": sim_rows},
    });
    store::write_json(&root.join("out").join("report.json"), &summary)?;
    // per-document scores (ids, strata and scores only)
    let mut w = store::ZWriter::create(&root.join("out").join("per-document.jsonl.zst"))?;
    for (i, d) in docs.iter().enumerate() {
        w.write(&json!({"id": d.id, "arm": d.arm, "host": d.host, "stratum": d.stratum, "domain": d.domain, "url_month": d.url_month,
            "crawl": d.crawl, "excluded": d.excluded, "a_involvement": if a0[i].is_finite() { json!(a0[i]) } else { Value::Null }}))?;
    }
    w.finish()?;
    Ok(json!({"twins": tf.n_a(), "frame": tf.n_b(), "strata": tf.strata.len(), "hosts": tf.hosts.len()}))
}

pub struct TauArgs<'a> {
    pub root: &'a Path,
    /// R0 (released seed 17) predictions on the twin set.
    pub a_pred: &'a Path,
    /// Honest models scored on the same set: (name, predictions).
    pub models: Vec<(String, std::path::PathBuf)>,
    /// The `report` step's report.json, for each model's composition part (optional).
    pub composition: Option<&'a Path>,
    pub boot: usize,
    pub sims: usize,
    pub sim_boot: usize,
    pub copy_risk_rule: bool,
}

/// Lower 5% quantiles of chi-square with k = 1..10 degrees of freedom.
const CHI2_05: [f64; 10] = [0.003932, 0.1026, 0.3518, 0.7107, 1.1455, 1.6354, 2.1673, 2.7326, 3.3251, 3.9403];

/// Honest-model offset tau measured directly: each honest model m scored on the twin set
/// gives `D_m = u_m - u_0 = a_0^2 - a_m^2` per (human) document and the canary statistic
/// `T_m = sum_s w_s (mean_frame D_m - mean_twin D_m)` against R0, with a host-bootstrap SE
/// and 95% interval. The panel gives tau-hat (95th percentile of |T|), the
/// noise-corrected systematic part and a chi-square upper bound. The canary's power is then
/// recomputed with the measured tau, and the canary is simulated with the real models as
/// honest baselines and as lookup attackers.
pub fn tau(a: &TauArgs) -> Result<Value> {
    ensure!(!a.models.is_empty() && a.models.len() <= CHI2_05.len(), "1 to {} models", CHI2_05.len());
    let Scored { mut docs, a0, failures, .. } = load_scored(a.root, a.a_pred, a.copy_risk_rule)?;
    let mut am: Vec<(String, Vec<f64>, usize)> = Vec::new();
    for (name, p) in &a.models {
        let (v, f) = involvement_by_doc(&docs, p)?;
        am.push((name.clone(), v, f));
    }
    let mut no_panel = 0;
    for (i, d) in docs.iter_mut().enumerate() {
        if d.excluded.is_none() && am.iter().any(|m| !m.1[i].is_finite()) {
            d.excluded = Some("no_panel_prediction".into());
            no_panel += 1;
        }
    }
    let tf = Design::new(&docs, "twin", "frame");
    ensure!(!tf.strata.is_empty(), "no matched strata");
    let (nt, nf) = (tf.n_a() as f64, tf.n_b() as f64);
    let matched: Vec<usize> = tf.strata.iter().flat_map(|s| s.1.iter().chain(s.2.iter()).copied().collect::<Vec<_>>()).collect();
    let loss0: Vec<f64> = a0.iter().map(|x| if x.is_finite() { x * x } else { 0.0 }).collect();
    let comp: Option<Value> = match a.composition {
        Some(p) => Some(store::read_json(p)?),
        None => None,
    };
    let arm_mean = |x: &[f64], k: usize| -> f64 { mean(&tf.strata.iter().flat_map(|s| if k == 0 { s.1.clone() } else { s.2.clone() }).map(|i| x[i]).collect::<Vec<_>>()) };
    let two_sample = (1.0 / nt + 1.0 / nf).sqrt();
    // R0's design factor (as in `report`), for power rows comparable with that table.
    let df_r0 = {
        let (_, _, se) = tf.boot(&loss0, 1000, 51);
        (se / (sd(&matched.iter().map(|&i| loss0[i]).collect::<Vec<_>>()) * two_sample)).powi(2)
    };
    let ell_h = {
        let mut x = vec![0.0; docs.len()];
        for s in &tf.strata {
            for &i in &s.2 {
                x[i] = loss0[i];
            }
        }
        tf.diff_b_minus_a(&x, &tf.all())
    };
    let mut models = serde_json::Map::new();
    let mut ts = Vec::new();
    let mut ses = Vec::new();
    let mut sigmas = Vec::new();
    let mut dfs = Vec::new();
    let mut d_by_model: Vec<Vec<f64>> = Vec::new();
    for (k, (name, v, f)) in am.iter().enumerate() {
        let d: Vec<f64> = (0..docs.len()).map(|i| if v[i].is_finite() && a0[i].is_finite() { loss0[i] - v[i] * v[i] } else { 0.0 }).collect();
        let lm: Vec<f64> = v.iter().map(|x| if x.is_finite() { x * x } else { 0.0 }).collect();
        let t = tf.diff_b_minus_a(&d, &tf.all());
        let (lo, hi, se) = tf.boot(&d, a.boot, 0x7A0 + k as u64);
        let iid = tf.iid_se(&d);
        let dm: Vec<f64> = matched.iter().map(|&i| d[i]).collect();
        let sigma = sd(&dm);
        let df = (se / (sigma * two_sample)).powi(2);
        let (glo, ghi, gse) = tf.boot(&lm, a.boot, 0x7B0 + k as u64);
        let shift: Vec<f64> = matched.iter().map(|&i| logit(v[i]) - logit(a0[i])).collect();
        let t_comp = comp.as_ref().and_then(|c| c["tier_c"]["panel"][name.as_str()]["T_composition"].as_f64());
        models.insert(name.clone(), json!({
            "failures": f,
            "T": t, "abs_T": t.abs(), "ci95_host_bootstrap": [lo, hi], "se_host_bootstrap": se, "se_independent": iid,
            "z": t / se, "abs_T_upper95": t.abs() + 1.96 * se,
            "sigma_doc_D": sigma, "design_factor_D": df, "mean_D_matched": mean(&dm),
            "mean_involvement": {"twin": arm_mean(v, 0), "frame": arm_mean(v, 1)},
            "mean_loss_a2": {"twin": arm_mean(&lm, 0), "frame": arm_mean(&lm, 1)},
            "own_loss_gap_frame_minus_twin": tf.diff_b_minus_a(&lm, &tf.all()), "own_loss_gap_ci95": [glo, ghi], "own_loss_gap_se": gse,
            "logit_shift_vs_R0": {"mean": mean(&shift), "sd": sd(&shift)},
            "T_composition_from_report": t_comp, "T_minus_composition": t_comp.map(|c| t - c),
        }));
        ts.push(t);
        ses.push(se);
        sigmas.push(sigma);
        dfs.push(df);
        d_by_model.push(d);
    }
    let mut abs: Vec<f64> = ts.iter().map(|t| t.abs()).collect();
    let tau_hat = percentile(&mut abs.clone(), 0.95);
    let max_abs = abs.iter().copied().fold(0.0, f64::max);
    abs.sort_by(f64::total_cmp);
    let m = ts.len() as f64;
    let rms = (ts.iter().map(|t| t * t).sum::<f64>() / m).sqrt();
    let mean_se2 = ses.iter().map(|s| s * s).sum::<f64>() / m;
    let systematic = (rms * rms - mean_se2).max(0.0).sqrt();
    let tau_upper = (ts.iter().map(|t| t * t).sum::<f64>() / CHI2_05[ts.len() - 1] - mean_se2).max(0.0).sqrt();
    let max_ci = am.iter().enumerate().map(|(k, _)| ts[k].abs() + 1.96 * ses[k]).fold(0.0, f64::max);
    let sigma_m = mean(&sigmas);
    let df_m = mean(&dfs);
    let panel = json!({"models": ts.len(), "tau_hat_p95_abs_T": tau_hat, "max_abs_T": max_abs, "rms_T": rms,
        "mean_se": mean_se2.sqrt(), "tau_systematic_noise_corrected": systematic,
        "tau_upper95_chi2": tau_upper, "chi2_rule": "sum T^2 / chi2_{k,0.05} - mean SE^2: the honest T_m taken as N(0, tau^2 + SE_m^2)",
        "max_abs_T_upper95": max_ci, "sigma_doc_D_mean": sigma_m, "design_factor_D_mean": df_m,
        "design_factor_R0_loss": df_r0, "ell_human_full_coverage": ell_h});

    // ---------------- power with the measured tau (close panel: these are all close fine-tunes)
    let child_gain_b = 2.0 * 0.0652 - 2.0 * 0.0353;
    let (sf_close, tauf_close) = (0.0519, 0.0048);
    let prev_sigma_close = comp.as_ref().and_then(|c| c["tier_c"]["sigma_doc_D_on_this_set"]["close"].as_f64());
    let taus = [(0.0, "zero"), (systematic, "measured: systematic part (noise-corrected rms of T)"),
                (rms, "measured: rms of T as realised on this set"), (tau_upper, "measured: 95% upper bound (chi-square)"),
                (0.0025, "sensitivity 0.0025"), (0.005, "sensitivity 0.005"), (tauf_close, "Tier C between-domain, families (lookup-v1)")];
    let mut power = Vec::new();
    for (nt2, nf2) in [(nt, nf), (2000.0, 2000.0), (20000.0, 20000.0)] {
        for &(tau, src) in &taus {
            power.push(power_row("human_document", "close (measured inputs)", sigma_m, tau, src, ell_h, nt2, nf2, df_m));
            if let Some(ps) = prev_sigma_close {
                power.push(power_row("human_document", "close (report inputs)", ps, tau, src, ell_h, nt2, nf2, df_r0));
            }
            for (strat, ell) in [("family_A", ell_h / 2.0), ("family_B", (ell_h + child_gain_b) / 2.0)] {
                power.push(power_row(strat, "close (lookup-v1 family sigma)", sf_close, tau, src, ell, nt2, nf2, df_r0));
            }
        }
    }

    // ---------------- canary simulated with the real models
    let coverages = [0.0, 0.05, 0.1, 0.2, 0.3, 0.5, 1.0];
    let rules = [("tau_hat = 95th percentile of |T| over the measured panel", tau_hat),
                 ("tau_hat = 1.96 x measured 95% upper bound of tau", 1.96 * tau_upper),
                 ("tau_hat = 0", 0.0)];
    let frame_set: Vec<bool> = {
        let mut f = vec![false; docs.len()];
        for s in &tf.strata {
            for &i in &s.2 {
                f[i] = true;
            }
        }
        f
    };
    let mut sim_rows = Vec::new();
    for (k, (name, v, _)) in am.iter().enumerate() {
        for &c in &coverages {
            let mut rng = Rng(0x7A57 ^ ((k as u64) << 32) ^ (c * 1000.0) as u64);
            let (mut tv, mut sv) = (Vec::new(), Vec::new());
            let runs = if c == 0.0 { 1 } else { a.sims };
            for _ in 0..runs {
                let mut d = d_by_model[k].clone();
                for &i in &matched {
                    if frame_set[i] && (rng.next_u64() as f64 / u64::MAX as f64) < c {
                        let x = v[i] * 1e-3;
                        d[i] = loss0[i] - x * x;
                    }
                }
                tv.push(tf.diff_b_minus_a(&d, &tf.all()));
                sv.push(tf.boot(&d, a.sim_boot, rng.next_u64()).2);
            }
            for (rule, th) in rules {
                let flags = tv.iter().zip(&sv).filter(|(t, se)| **t - th > Z_ALPHA * **se).count();
                sim_rows.push(json!({"model": name, "coverage": c, "tau_rule": rule, "tau_hat": th, "runs": runs,
                    "flag_rate": flags as f64 / runs as f64, "mean_T": mean(&tv), "mean_se": mean(&sv)}));
            }
        }
    }
    let out = json!({
        "schema": "slop_ninja_cc_twins_tau_v1",
        "statistic": "human-document lookup-canary statistic against R0: D = u_m - u_0, u = 1 - a^2 (all twin-set documents are human); T = sum_s w_s (mean_frame D - mean_twin D), w_s = stratum share of twins; SE and 95% interval from a host bootstrap",
        "r0": {"predictions": a.a_pred, "failures": failures},
        "matched": {"twins": tf.n_a(), "frame": tf.n_b(), "strata": tf.strata.len(), "hosts": tf.hosts.len(), "no_panel_prediction": no_panel},
        "boot": a.boot, "models": models, "panel": panel,
        "power": power,
        "simulation": {"rule": "attacker = the honest model with a x 1e-3 on a random fraction c of frame documents; flag when T - tau_hat > 3.09 SE (host bootstrap); coverage 0 is the model itself (deterministic)", "sims": a.sims, "boot": a.sim_boot, "rows": sim_rows},
    });
    store::write_json(&a.root.join("tau").join("out").join("tau.json"), &out)?;
    Ok(json!({"models": ts.len(), "T": ts, "tau_hat_p95_abs_T": tau_hat, "tau_systematic": systematic, "tau_upper95": tau_upper}))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(id: &str, arm: &str, host: &str, month: &str) -> Doc {
        let mut d: Doc = serde_json::from_value(json!({
            "id": id, "arm": arm, "range": 0, "host": host, "registered_domain": host, "group": id,
            "stratum": format!("{host}|{month}"), "url_month": month, "url": format!("https://{host}/{month}/{id}"),
            "crawl": "CC-MAIN-2023-06", "warc_date": "2023-01-01T00:00:00Z", "record_id": id, "payload_digest": null,
            "filename": "f", "offset": 0, "length": 1, "page_published": null, "page_published_source": null,
            "page_modified": null, "charset": "utf-8", "page_sha256": "x", "page_words": 200, "domain": "news",
            "domain_rule": "r", "page_domain": "news", "line_start": 0, "line_end": 1, "valid_runs": 1, "words": 200,
            "text": format!("Synthetic passage {id} about a quiet harbour town."), "text_sha256": "x", "folded_sha256": "x",
            "text_redacted": null, "pair": null, "frame_crawls_seen": 0, "first_frame_ts": null, "excluded": null}))
        .unwrap();
        d.text_sha256 = crate::measure::sha256_hex(d.text.as_bytes());
        d
    }

    #[test]
    fn statistic_is_stratum_weighted_frame_minus_twin_of_d() {
        // Two strata on two hosts: (2 twins, 1 frame) and (1 twin, 1 frame).
        let docs = vec![doc("t1", "twin", "a", "2019-01"), doc("t2", "twin", "a", "2019-01"), doc("f1", "frame", "a", "2019-01"),
                        doc("t3", "twin", "b", "2020-05"), doc("f2", "frame", "b", "2020-05")];
        let tf = Design::new(&docs, "twin", "frame");
        assert_eq!((tf.n_a(), tf.n_b(), tf.strata.len(), tf.hosts.len()), (3, 2, 2, 2));
        let d = [0.0, 0.2, 0.4, 0.1, 0.0];
        // w = (2/3, 1/3); stratum a: 0.4 - 0.1; stratum b: 0.0 - 0.1.
        let t = tf.diff_b_minus_a(&d, &tf.all());
        assert!((t - (2.0 / 3.0 * 0.3 + 1.0 / 3.0 * -0.1)).abs() < 1e-12, "{t}");
        // A model equal to R0 has D = 0 and T = 0 with zero bootstrap spread.
        let (lo, hi, se) = tf.boot(&[0.0; 5], 50, 1);
        assert_eq!((lo, hi, se), (0.0, 0.0, 0.0));
    }

    #[test]
    fn tau_joins_models_by_text_and_writes_the_panel() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::create_dir_all(root.join("extract")).unwrap();
        let mut docs = Vec::new();
        for h in 0..6 {
            for k in 0..3 {
                docs.push(doc(&format!("t{h}{k}"), "twin", &format!("h{h}"), "2019-03"));
                docs.push(doc(&format!("f{h}{k}"), "frame", &format!("h{h}"), "2019-03"));
            }
        }
        let mut w = store::ZWriter::create(&root.join("extract").join("docs.jsonl.zst")).unwrap();
        for d in &docs {
            w.write(&serde_json::to_value(d).unwrap()).unwrap();
        }
        w.finish().unwrap();
        let pred = |name: &str, f: &dyn Fn(&Doc) -> f64| {
            let p = root.join(format!("{name}.jsonl"));
            let s: String = docs.iter().map(|d| format!("{}\n", json!({"id": d.id, "status": "ok",
                "text_sha256": crate::measure::sha256_hex(d.text.as_bytes()),
                "prediction": {"attribution": {"ai": {"involvement_probability": f(d)}}}}))).collect();
            fs::write(&p, s).unwrap();
            p
        };
        let r0 = pred("r0", &|_| 0.3);
        // Same as R0 on twins, half the involvement on frame documents: D = 0.09 - 0.0225 on frame.
        let m = pred("m", &|d| if d.arm == "frame" { 0.15 } else { 0.3 });
        let same = pred("same", &|_| 0.3);
        let v = tau(&TauArgs { root, a_pred: &r0, models: vec![("m".into(), m), ("same".into(), same)], composition: None,
                               boot: 200, sims: 4, sim_boot: 20, copy_risk_rule: false }).unwrap();
        let t = v["T"].as_array().unwrap();
        assert!((t[0].as_f64().unwrap() - (0.09 - 0.0225)).abs() < 1e-12);
        assert_eq!(t[1].as_f64().unwrap(), 0.0);
        let out: Value = store::read_json(&root.join("tau").join("out").join("tau.json")).unwrap();
        assert_eq!(out["matched"]["twins"], 18);
        assert_eq!(out["models"]["same"]["T"], 0.0);
        // A text that differs from the scored document is refused.
        let bad = root.join("bad.jsonl");
        fs::write(&bad, format!("{}\n", json!({"id": "t00", "status": "ok", "text_sha256": "0",
            "prediction": {"attribution": {"ai": {"involvement_probability": 0.1}}}}))).unwrap();
        assert!(tau(&TauArgs { root, a_pred: &r0, models: vec![("bad".into(), bad)], composition: None,
                               boot: 10, sims: 1, sim_boot: 5, copy_risk_rule: false }).is_err());
    }
}
