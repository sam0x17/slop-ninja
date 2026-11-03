//! Training-author selection (PROTOCOL.md section 5), the single evaluation of
//! the frozen arms (sections 6-7), the paired author bootstrap (section 8) and
//! the decision rule (section 9). Protocol v2 adds arm F (PROTOCOL-v2.md
//! section 4), the F - D contrast and the v2 decision (sections 5-6); the v1
//! arms, record, contrasts and decision are computed by the v1 code unchanged.
use crate::common::*;
use crate::load::*;
use crate::space::*;
use crate::stats::*;
use crate::tasks::*;
use crate::v2::{PRIMARY_METRIC, PairSink, logistic_fit3};
use anyhow::{Context, Result, ensure};
use serde_json::{Map, Value, json};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::Path;

pub const ARMS: [char; 5] = ['A', 'B', 'C', 'D', 'E'];
/// Protocol v2 arms: v1's and F.
pub const ARMS_V2: [char; 6] = ['A', 'B', 'C', 'D', 'E', 'F'];
pub const BASE: [char; 3] = ['A', 'B', 'C'];
/// (fused arm, first component, second component)
pub const FUSIONS: [(char, char, char); 2] = [('D', 'B', 'C'), ('E', 'A', 'B')];
/// Protocol v2 arm F and its components, in the frozen order B, C, A.
pub const FUSION_F: (char, [char; 3]) = ('F', ['B', 'C', 'A']);
pub const LAMBDA: f64 = 1e-3;
pub const REPLICATES: usize = 2000;
pub const SEED_STRING: &str = "cfg-vs-encoder-20261031/bootstrap/v1";
pub const TIE: f64 = 1e-12;
pub const CONTRASTS: [(char, char); 6] =
    [('A', 'B'), ('E', 'B'), ('D', 'B'), ('C', 'B'), ('E', 'A'), ('A', 'C')];
/// Protocol v2 contrasts: v1's and the single added contrast F - D.
pub const CONTRASTS_V2: [(char, char); 7] =
    [('A', 'B'), ('E', 'B'), ('D', 'B'), ('C', 'B'), ('E', 'A'), ('A', 'C'), ('F', 'D')];

fn s(c: char) -> String {
    c.to_string()
}

pub struct Trained {
    pub models: BTreeMap<char, Model>,
    pub znorm: BTreeMap<char, [(f64, f64); 3]>,
    pub fusion: BTreeMap<char, [f64; 3]>,
    pub thresholds: BTreeMap<char, [f64; 3]>,
    pub record: Value,
    /// Arm F's [b0, b_B, b_C, b_A]; None when its fit failed.
    pub fusion_f: Option<[f64; 4]>,
    /// Protocol v2 training-phase additions (arm F), kept out of `record`.
    pub record_v2: Value,
}

/// Element-wise map of three score matrices on one population.
pub fn mat_map3(a: &Mat, b: &Mat, c: &Mat, f: impl Fn(f64, f64, f64) -> f64) -> Mat {
    Mat {
        per_dom: a
            .per_dom
            .iter()
            .zip(&b.per_dom)
            .zip(&c.per_dom)
            .map(|((x, y), z)| {
                assert!(x.len() == y.len() && y.len() == z.len(), "score matrix shape");
                x.iter().zip(y).zip(z).map(|((p, q), r)| f(*p, *q, *r)).collect()
            })
            .collect(),
    }
}

/// Element-wise map of three T3 score sets on one stratum.
pub fn t3_map3(a: &T3Scores, b: &T3Scores, c: &T3Scores, f: impl Fn(f64, f64, f64) -> f64) -> T3Scores {
    let m = |x: &Vec<Vec<f64>>, y: &Vec<Vec<f64>>, z: &Vec<Vec<f64>>| -> Vec<Vec<f64>> {
        x.iter()
            .zip(y)
            .zip(z)
            .map(|((p, q), r)| {
                assert!(p.len() == q.len() && q.len() == r.len(), "T3 score shape");
                p.iter().zip(q).zip(r).map(|((s, t), u)| f(*s, *t, *u)).collect()
            })
            .collect()
    };
    T3Scores {
        rec: m(&a.rec, &b.rec, &c.rec),
        src: m(&a.src, &b.src, &c.src),
    }
}

fn raws_for(
    inp: &Inputs,
    rep: Rep,
    text: Text,
    docs: &BTreeSet<usize>,
    threads: usize,
) -> Result<HashMap<usize, Sparse>> {
    let list: Vec<usize> = docs.iter().copied().collect();
    let out: Vec<Result<Sparse>> = par_map(list.len(), threads, |i| raw(inp, list[i], rep, text));
    let mut m = HashMap::new();
    for (d, r) in list.into_iter().zip(out) {
        m.insert(d, r?);
    }
    Ok(m)
}

fn prepare_from(
    model: &Model,
    docs: &BTreeSet<usize>,
    raws: &HashMap<usize, Sparse>,
) -> Result<HashMap<usize, Prepared>> {
    let mut m = HashMap::new();
    for &d in docs {
        let p = model.prepare(&raws[&d]);
        ensure!(p.n.is_finite() && p.n > 0.0, "degenerate standardized document");
        m.insert(d, p);
    }
    Ok(m)
}

fn mean(v: &[f64]) -> f64 {
    v.iter().sum::<f64>() / v.len() as f64
}

impl Trained {
    pub fn fuse(&self, arm: char, ki: usize, s1: f64, s2: f64) -> f64 {
        let (_, c1, c2) = FUSIONS.iter().find(|f| f.0 == arm).copied().unwrap();
        let b = self.fusion[&arm];
        let (m1, d1) = self.znorm[&c1][ki];
        let (m2, d2) = self.znorm[&c2][ki];
        b[0] + b[1] * (s1 - m1) / d1 + b[2] * (s2 - m2) / d2
    }

    pub fn add_fused_mats(&self, mats: &mut BTreeMap<char, Vec<Mat>>) {
        for (f, c1, c2) in FUSIONS {
            let v: Vec<Mat> = (0..KS.len())
                .map(|ki| Mat::map2(&mats[&c1][ki], &mats[&c2][ki], |a, b| self.fuse(f, ki, a, b)))
                .collect();
            mats.insert(f, v);
        }
        if self.fusion_f.is_some() {
            let v = self.fused_f_mats(mats);
            mats.insert(FUSION_F.0, v);
        }
    }

    pub fn add_fused_t3(&self, sc: &mut BTreeMap<char, T3Scores>) {
        for (f, c1, c2) in FUSIONS {
            let v = T3Scores::map2(&sc[&c1], &sc[&c2], |a, b| self.fuse(f, 2, a, b));
            sc.insert(f, v);
        }
        if self.fusion_f.is_some() {
            let [x, y, z] = FUSION_F.1;
            let v = t3_map3(&sc[&x], &sc[&y], &sc[&z], |a, b, c| self.fuse_f(2, [a, b, c]));
            sc.insert(FUSION_F.0, v);
        }
    }

    /// Arm F at k index `ki`: b0 + b_B z_B + b_C z_C + b_A z_A, with each
    /// component z-normalized by v1's impostor constants at that k.
    pub fn fuse_f(&self, ki: usize, s: [f64; 3]) -> f64 {
        let b = self.fusion_f.expect("arm F is fitted");
        let mut x = b[0];
        for (j, c) in FUSION_F.1.iter().enumerate() {
            let (m, d) = self.znorm[c][ki];
            x += b[j + 1] * (s[j] - m) / d;
        }
        x
    }

    pub fn fused_f_mats(&self, mats: &BTreeMap<char, Vec<Mat>>) -> Vec<Mat> {
        let [x, y, z] = FUSION_F.1;
        (0..KS.len())
            .map(|ki| mat_map3(&mats[&x][ki], &mats[&y][ki], &mats[&z][ki], |a, b, c| self.fuse_f(ki, [a, b, c])))
            .collect()
    }
}

pub fn pop_summary(pop: &Pop) -> Value {
    let mut by = Map::new();
    for (d, name) in pop.domains.iter().enumerate() {
        let a = &pop.dom_authors[d];
        by.insert(
            name.clone(),
            json!({"authors":a.len(),"withheld":a.iter().filter(|&&i| pop.authors[i].withheld).count(),
                "queries":pop.dom_queries[d].len()}),
        );
    }
    json!({"authors":pop.authors.len(),"queries":pop.queries.len(),"documents":pop.all_docs().len(),"by_domain":by})
}

/// Section 5 on training authors only: 5-fold selection, out-of-fold fusion,
/// refit on all training authors, T2 rejection thresholds.
pub fn train(inp: &Inputs, pop: &Pop, text: Text, threads: usize) -> Result<Trained> {
    let fold_train: Vec<Pop> = (0..5).map(|f| pop.subset(|a| a.fold != f)).collect();
    let fold_held: Vec<Pop> = (0..5).map(|f| pop.subset(|a| a.fold == f)).collect();
    let fold_imps: Vec<Vec<Vec<usize>>> = fold_held.iter().map(|p| impostor_sets(inp, p)).collect();
    let all_docs = pop.all_docs();
    let mut cv_rows: Vec<(char, Config, Vec<f64>, Vec<usize>)> = Vec::new();
    for arm in BASE {
        for rep in reps(arm) {
            let raws = raws_for(inp, rep, text, &all_docs, threads)?;
            let mut aucs: BTreeMap<Scorer, Vec<f64>> = BTreeMap::new();
            let mut dims = Vec::new();
            for f in 0..5 {
                let (docs, groups) = fold_train[f].training_docs();
                let refs: Vec<&Sparse> = docs.iter().map(|d| &raws[d]).collect();
                let st = fit_stats(inp, rep, &refs, &groups);
                dims.push(st.vocab.len());
                let held_docs = fold_held[f].all_docs();
                for scorer in [Scorer::S1, Scorer::S2] {
                    let model = Model::new(Config { rep, scorer }, text, &st);
                    let prep = prepare_from(&model, &held_docs, &raws)?;
                    let mats = score_pop(&model, &fold_held[f], &prep, &[5], threads);
                    let items = t1_items(&fold_held[f], &mats[0], &fold_imps[f], &|_| true);
                    let auc = Roc::new(items).eval(None).auc;
                    ensure!(auc.is_finite(), "undefined training-fold AUC");
                    aucs.entry(scorer).or_default().push(auc);
                }
            }
            for scorer in [Scorer::S1, Scorer::S2] {
                cv_rows.push((arm, Config { rep, scorer }, aucs[&scorer].clone(), dims.clone()));
            }
            eprintln!("[{}] cv {} {}", text.name(), arm, rep.name());
        }
    }
    let mut selected: BTreeMap<char, (Config, f64)> = BTreeMap::new();
    for arm in BASE {
        let order = candidates(arm);
        let mut best: Option<(Config, f64)> = None;
        for c in &order {
            let row = cv_rows.iter().find(|r| r.0 == arm && r.1 == *c).unwrap();
            let m = mean(&row.2);
            if best.is_none_or(|b| m > b.1 + TIE) {
                best = Some((*c, m));
            }
        }
        selected.insert(arm, best.unwrap());
    }

    // Out-of-fold scores of the selected configurations at every k.
    let mut imp: BTreeMap<char, [Vec<(f64, f64)>; 3]> = BTreeMap::new();
    let mut k5: BTreeMap<char, Vec<Item>> = BTreeMap::new();
    for arm in BASE {
        let cfg = selected[&arm].0;
        let raws = raws_for(inp, cfg.rep, text, &all_docs, threads)?;
        let mut slots: [Vec<(f64, f64)>; 3] = Default::default();
        let mut items5 = Vec::new();
        for f in 0..5 {
            let (docs, groups) = fold_train[f].training_docs();
            let refs: Vec<&Sparse> = docs.iter().map(|d| &raws[d]).collect();
            let st = fit_stats(inp, cfg.rep, &refs, &groups);
            let model = Model::new(cfg, text, &st);
            let prep = prepare_from(&model, &fold_held[f].all_docs(), &raws)?;
            let mats = score_pop(&model, &fold_held[f], &prep, &KS, threads);
            for ki in 0..KS.len() {
                let items = t1_items(&fold_held[f], &mats[ki], &fold_imps[f], &|_| true);
                slots[ki].extend(items.iter().filter(|i| !i.pos).map(|i| (i.s, i.w)));
                if ki == 2 {
                    items5.extend(items);
                }
            }
        }
        imp.insert(arm, slots);
        k5.insert(arm, items5);
    }
    let mut znorm: BTreeMap<char, [(f64, f64); 3]> = BTreeMap::new();
    for arm in BASE {
        let z: Vec<(f64, f64)> = imp[&arm].iter().map(|v| weighted_mean_sd(v)).collect();
        ensure!(z.iter().all(|(m, d)| m.is_finite() && *d > 0.0), "impostor normalization");
        znorm.insert(arm, [z[0], z[1], z[2]]);
    }
    let mut fusion: BTreeMap<char, [f64; 3]> = BTreeMap::new();
    let mut fusion_rec = Map::new();
    for (f, c1, c2) in FUSIONS {
        let (a, b) = (&k5[&c1], &k5[&c2]);
        ensure!(a.len() == b.len(), "fusion trial alignment");
        let mut x = Vec::with_capacity(a.len());
        let mut y = Vec::with_capacity(a.len());
        let mut w = Vec::with_capacity(a.len());
        for (p, q) in a.iter().zip(b) {
            ensure!(p.pos == q.pos && p.unit == q.unit && p.w == q.w, "fusion trial alignment");
            let (m1, d1) = znorm[&c1][2];
            let (m2, d2) = znorm[&c2][2];
            x.push([(p.s - m1) / d1, (q.s - m2) / d2]);
            y.push(p.pos);
            w.push(p.w);
        }
        let beta = logistic_fit(&x, &y, &w, LAMBDA)?;
        fusion.insert(f, beta);
        fusion_rec.insert(
            s(f),
            json!({"components":[s(c1),s(c2)],"beta":beta,"trials":x.len(),
                "genuine_trials":y.iter().filter(|p| **p).count()}),
        );
    }

    // Refit on all training authors.
    let (docs, groups) = pop.training_docs();
    let mut models: BTreeMap<char, Model> = BTreeMap::new();
    for arm in BASE {
        let cfg = selected[&arm].0;
        let raws = raws_for(inp, cfg.rep, text, &all_docs, threads)?;
        let refs: Vec<&Sparse> = docs.iter().map(|d| &raws[d]).collect();
        let st = fit_stats(inp, cfg.rep, &refs, &groups);
        models.insert(arm, Model::new(cfg, text, &st));
    }
    let mut tr = Trained {
        models,
        znorm,
        fusion,
        thresholds: BTreeMap::new(),
        record: Value::Null,
        fusion_f: None,
        record_v2: Value::Null,
    };

    // T2 rejection thresholds: simulated-withheld training authors against
    // same-domain training galleries, statistics refit on all training authors.
    let mut mats: BTreeMap<char, Vec<Mat>> = BTreeMap::new();
    for arm in BASE {
        let model = &tr.models[&arm];
        let prep = prepare_docs(inp, model, &all_docs, threads)?;
        mats.insert(arm, score_pop(model, pop, &prep, &KS, threads));
    }
    tr.add_fused_mats(&mut mats);
    let mut withheld_queries = 0usize;
    for arm in ARMS {
        let mut t = [0.0; 3];
        for ki in 0..KS.len() {
            let qs = t2_queries(pop, &mats[&arm][ki]);
            withheld_queries = qs.iter().filter(|q| !q.enrolled).count();
            t[ki] = t2_threshold(&qs);
            ensure!(t[ki].is_finite(), "T2 threshold");
        }
        tr.thresholds.insert(arm, t);
    }

    let mut cv = Map::new();
    for arm in BASE {
        let rows: Vec<Value> = cv_rows
            .iter()
            .filter(|r| r.0 == arm)
            .map(|r| json!({"config":r.1.name(),"fold_auc_k5":r.2,"mean_auc_k5":mean(&r.2),"fold_dimensions":r.3}))
            .collect();
        cv.insert(s(arm), json!(rows));
    }
    tr.record = json!({
        "text":text.name(),
        "training_population":pop_summary(pop),
        "fold_authors":fold_held.iter().map(|p| p.authors.len()).collect::<Vec<_>>(),
        "cross_validation":cv,
        "selected":selected.iter().map(|(a,(c,m))| (s(*a), json!({"config":c.name(),"mean_auc_k5":m}))).collect::<Map<_,_>>(),
        "refit_models":tr.models.iter().map(|(a,m)| (s(*a), m.summary())).collect::<Map<_,_>>(),
        "impostor_normalization":tr.znorm.iter().map(|(a,z)| (s(*a), json!(KS.iter().zip(z).map(|(k,(m,d))| json!({"k":k,"mean":m,"sd":d})).collect::<Vec<_>>()))).collect::<Map<_,_>>(),
        "fusion":fusion_rec,
        "t2_thresholds":tr.thresholds.iter().map(|(a,t)| (s(*a), json!(t))).collect::<Map<_,_>>(),
        "t2_threshold_withheld_queries":withheld_queries,
    });

    // Protocol v2, section 4: arm F on the same out-of-fold k = 5 trials,
    // fitted after the v1 record is complete, which therefore is unchanged.
    let (fa, comps) = FUSION_F;
    let (a, b, c) = (&k5[&comps[0]], &k5[&comps[1]], &k5[&comps[2]]);
    ensure!(a.len() == b.len() && b.len() == c.len(), "F fusion trial alignment");
    let mut x = Vec::with_capacity(a.len());
    let mut y = Vec::with_capacity(a.len());
    let mut w = Vec::with_capacity(a.len());
    let z = |it: &Item, comp: char| -> f64 {
        let (m, d) = tr.znorm[&comp][2];
        (it.s - m) / d
    };
    for ((p, q), r) in a.iter().zip(b).zip(c) {
        ensure!(
            p.pos == q.pos && p.pos == r.pos && p.unit == q.unit && p.unit == r.unit && p.w == q.w && p.w == r.w,
            "F fusion trial alignment"
        );
        x.push([z(p, comps[0]), z(q, comps[1]), z(r, comps[2])]);
        y.push(p.pos);
        w.push(p.w);
    }
    let genuine = y.iter().filter(|p| **p).count();
    let components: Vec<String> = comps.iter().map(|c| s(*c)).collect();
    tr.record_v2 = match logistic_fit3(&x, &y, &w, LAMBDA) {
        Ok(fit) => {
            tr.fusion_f = Some(fit.beta);
            let fm = tr.fused_f_mats(&mats);
            let mut t = [0.0; 3];
            for ki in 0..KS.len() {
                t[ki] = t2_threshold(&t2_queries(pop, &fm[ki]));
                ensure!(t[ki].is_finite(), "T2 threshold");
            }
            tr.thresholds.insert(fa, t);
            json!({"arm":s(fa),"available":true,"text":text.name(),
                "fusion":{"components":components,"beta":fit.beta,"beta_terms":["b0","b_B","b_C","b_A"],
                    "trials":x.len(),"genuine_trials":genuine,"lambda":LAMBDA,
                    "newton_iterations":fit.iterations,"converged":fit.converged,"last_max_abs_step":fit.last_max_abs_step},
                "t2_thresholds":t})
        }
        Err(e) => json!({"arm":s(fa),"available":false,"text":text.name(),
            "fusion":{"components":components,"trials":x.len(),"genuine_trials":genuine,"lambda":LAMBDA,"failure":e.to_string()}}),
    };
    Ok(tr)
}

pub struct PopScores {
    pub mats: BTreeMap<char, Vec<Mat>>,
    pub imps: Vec<Vec<usize>>,
}

pub fn score_t12(inp: &Inputs, tr: &Trained, pop: &Pop, threads: usize) -> Result<PopScores> {
    let docs = pop.all_docs();
    let mut mats = BTreeMap::new();
    for arm in BASE {
        let model = &tr.models[&arm];
        let prep = prepare_docs(inp, model, &docs, threads)?;
        mats.insert(arm, score_pop(model, pop, &prep, &KS, threads));
    }
    tr.add_fused_mats(&mut mats);
    for v in mats.values() {
        for m in v {
            ensure!(m.per_dom.iter().flatten().all(|x| x.is_finite()), "nonfinite score");
        }
    }
    Ok(PopScores {
        mats,
        imps: impostor_sets(inp, pop),
    })
}

pub fn score_t3_all(
    inp: &Inputs,
    tr: &Trained,
    st: &T3Stratum,
    threads: usize,
) -> Result<BTreeMap<char, T3Scores>> {
    let docs = st.docs();
    let mut sc = BTreeMap::new();
    for arm in BASE {
        let model = &tr.models[&arm];
        let prep = prepare_docs(inp, model, &docs, threads)?;
        sc.insert(arm, score_t3(model, st, &prep, threads));
    }
    tr.add_fused_t3(&mut sc);
    Ok(sc)
}

fn write_t12_pairs(
    inp: &Inputs,
    mut w: PairSink,
    pop: &Pop,
    sets: &[(&str, &PopScores)],
    arms: &[char],
) -> Result<(usize, String)> {
    let imps: Vec<BTreeSet<usize>> = sets[0].1.imps.iter().map(|v| v.iter().copied().collect()).collect();
    for d in 0..pop.domains.len() {
        for &q in &pop.dom_queries[d] {
            let (qa, qdoc) = pop.queries[q];
            for &a in &pop.dom_authors[d] {
                let mut scores = Map::new();
                for (label, ps) in sets {
                    let mut by = Map::new();
                    for &arm in arms {
                        let Some(m) = ps.mats.get(&arm) else { continue };
                        let v: Vec<f64> = (0..KS.len()).map(|ki| m[ki].get(pop, q, a)).collect();
                        by.insert(s(arm), json!(v));
                    }
                    scores.insert(label.to_string(), Value::Object(by));
                }
                w.write(&json!({"domain":pop.domains[d],"query_id":inp.docs[qdoc].id,
                    "query_writer":pop.authors[qa].writer,"query_withheld":pop.authors[qa].withheld,
                    "query_words":inp.docs[qdoc].words,
                    "profile_writer":pop.authors[a].writer,"profile_enrolled":!pop.authors[a].withheld,
                    "genuine":a == qa,"t1_impostor":imps[q].contains(&a),"k":KS,"scores":scores}))?;
            }
        }
    }
    w.finish()
}

fn write_t3_pairs(
    inp: &Inputs,
    mut w: PairSink,
    st: &T3Stratum,
    sc: &BTreeMap<char, T3Scores>,
    arms: &[char],
) -> Result<(usize, String)> {
    for (t, tg) in st.targets.iter().enumerate() {
        for (i, r) in st.records.iter().enumerate() {
            let scores: Map<String, Value> = arms.iter().map(|a| (s(*a), json!(sc[a].rec[t][i]))).collect();
            let negative = r.target != t && r.parent_writer.as_deref() != Some(tg.writer.as_str());
            w.write(&json!({"stratum":st.name,"target":tg.writer,"doc_id":inp.docs[r.doc].id,"role":"record",
                "record_target":st.targets[r.target].writer,"kind":r.kind,"family":r.family,
                "positive":r.target == t,"negative":negative,"k":5,"scores":scores}))?;
        }
        for (j, &src) in tg.sources.iter().enumerate() {
            let scores: Map<String, Value> = arms.iter().map(|a| (s(*a), json!(sc[a].src[t][j]))).collect();
            w.write(&json!({"stratum":st.name,"target":tg.writer,"doc_id":inp.docs[src].id,"role":"own_source",
                "positive":false,"negative":true,"k":5,"scores":scores}))?;
        }
    }
    w.finish()
}

// ------------------------------------------------------------ bootstrap

/// Multiplicity source of a block: T1/T2 population (0 primary, 1 restricted
/// subset mapped to primary authors) or a T3 stratum.
#[derive(Clone, Copy)]
enum Units {
    Primary,
    Restricted,
    T3(usize),
}

enum Block {
    Roc {
        units: Units,
        roc: Roc,
        slots: [Option<usize>; 4],
    },
    Mean {
        units: Units,
        vals: Vec<f64>,
        slot: usize,
    },
}

#[derive(Default)]
struct Registry {
    names: Vec<(String, char)>,
    index: HashMap<(String, char), usize>,
}

impl Registry {
    fn slot(&mut self, name: &str, arm: char) -> usize {
        let key = (name.to_string(), arm);
        if let Some(&i) = self.index.get(&key) {
            return i;
        }
        let i = self.names.len();
        self.names.push(key.clone());
        self.index.insert(key, i);
        i
    }
}

#[allow(clippy::too_many_arguments)]
fn t1_blocks(
    reg: &mut Registry,
    blocks: &mut Vec<Block>,
    label: &str,
    units: Units,
    inp: &Inputs,
    pop: &Pop,
    ps: &PopScores,
    arms: &[char],
) {
    let mut strata: Vec<(String, Box<dyn Fn(usize) -> bool + '_>)> = vec![("pooled".into(), Box::new(|_| true))];
    for (d, name) in pop.domains.iter().enumerate() {
        strata.push((format!("domain:{name}"), Box::new(move |q| pop.authors[pop.queries[q].0].domain == d)));
    }
    for (lo, hi, name) in LENGTH_STRATA {
        strata.push((
            format!("length:{name}"),
            Box::new(move |q| {
                let w = inp.docs[pop.queries[q].1].words;
                w >= lo && w <= hi
            }),
        ));
    }
    for &arm in arms {
        let Some(mats) = ps.mats.get(&arm) else { continue };
        for (ki, k) in KS.iter().enumerate() {
            for (sname, keep) in &strata {
                let items = t1_items(pop, &mats[ki], &ps.imps, keep.as_ref());
                if items.is_empty() {
                    continue;
                }
                let base = format!("{label}/T1/{sname}/k{k}");
                let slots = ["auc", "eer", "fpr_at_tpr_0.90", "fpr_at_tpr_0.95"]
                    .map(|m| Some(reg.slot(&format!("{base}/{m}"), arm)));
                blocks.push(Block::Roc {
                    units,
                    roc: Roc::new(items),
                    slots,
                });
            }
        }
    }
}

fn t2_blocks(reg: &mut Registry, blocks: &mut Vec<Block>, tr: &Trained, pop: &Pop, ps: &PopScores, arms: &[char]) {
    let mut scopes: Vec<(String, Option<usize>)> = vec![("pooled".into(), None)];
    for (d, name) in pop.domains.iter().enumerate() {
        scopes.push((format!("domain:{name}"), Some(d)));
    }
    for &arm in arms {
        for (ki, k) in KS.iter().enumerate() {
            let thr = tr.thresholds[&arm][ki];
            let qs = t2_queries(pop, &ps.mats[&arm][ki]);
            for (sname, dom) in &scopes {
                let in_scope = |a: usize| dom.is_none_or(|d| pop.authors[a].domain == d);
                let n = pop.authors.len();
                let mut acc = vec![[0.0f64; 4]; n]; // top1, rr, dir, far sums
                let mut cnt = vec![0.0f64; n];
                for q in &qs {
                    if !in_scope(q.author) {
                        continue;
                    }
                    let a = &mut acc[q.author];
                    cnt[q.author] += 1.0;
                    if q.enrolled {
                        a[0] += q.top1 as u8 as f64;
                        a[1] += q.rr;
                        a[2] += (q.top1 && q.max >= thr) as u8 as f64;
                    } else {
                        a[3] += (q.max >= thr) as u8 as f64;
                    }
                }
                let base = format!("primary/T2/{sname}/k{k}");
                for (m, name) in ["top1", "mrr", "detection_identification_rate", "false_accept_rate"]
                    .iter()
                    .enumerate()
                {
                    let vals: Vec<f64> = (0..n)
                        .map(|a| {
                            let applicable = cnt[a] > 0.0 && ((m < 3) == !pop.authors[a].withheld);
                            if applicable { acc[a][m] / cnt[a] } else { f64::NAN }
                        })
                        .collect();
                    blocks.push(Block::Mean {
                        units: Units::Primary,
                        vals,
                        slot: reg.slot(&format!("{base}/{name}"), arm),
                    });
                }
                let items: Vec<Item> = qs
                    .iter()
                    .filter(|q| in_scope(q.author))
                    .map(|q| Item {
                        s: q.max,
                        pos: q.enrolled,
                        unit: q.author as u32,
                        w: 1.0 / cnt[q.author],
                    })
                    .collect();
                blocks.push(Block::Roc {
                    units: Units::Primary,
                    roc: Roc::new(items),
                    slots: [Some(reg.slot(&format!("{base}/auroc_max_enrolled_vs_withheld"), arm)), None, None, None],
                });
            }
        }
    }
}

fn unit_mult(mult: Option<&[Vec<f64>]>, u: Units) -> Option<&[f64]> {
    mult.map(|m| match u {
        Units::Primary => m[0].as_slice(),
        Units::Restricted => m[1].as_slice(),
        Units::T3(s) => m[2 + s].as_slice(),
    })
}

fn eval_block(b: &Block, mult: Option<&[Vec<f64>]>, out: &mut [f64]) {
    let m = |u: Units| unit_mult(mult, u);
    match b {
        Block::Roc { units, roc, slots } => {
            let r = roc.eval(m(*units));
            for (slot, v) in slots.iter().zip([r.auc, r.eer, r.fpr90, r.fpr95]) {
                if let Some(i) = slot {
                    out[*i] = v;
                }
            }
        }
        Block::Mean { units, vals, slot } => {
            let mm = m(*units);
            let (mut num, mut den) = (0.0, 0.0);
            for (i, v) in vals.iter().enumerate() {
                if v.is_finite() {
                    let w = mm.map_or(1.0, |x| x[i]);
                    num += w * v;
                    den += w;
                }
            }
            out[*slot] = if den > 0.0 { num / den } else { f64::NAN };
        }
    }
}

fn summary_json(point: f64, reps: &[f64], with975: bool) -> Value {
    let sm = summarize(reps);
    let mut v = json!({"point":point,"ci95":[sm.ci95.0,sm.ci95.1],"se":sm.se,"smallest_detectable_difference":1.96*sm.se,
        "valid_replicates":sm.n});
    if with975 {
        v["ci97_5"] = json!([sm.ci975.0, sm.ci975.1]);
    }
    v
}

pub struct EvalInputs<'a> {
    pub label: &'a str,
    pub trained: &'a Trained,
    pub trained_restricted: &'a Trained,
    pub pop: &'a Pop,
    pub pop_restricted: &'a Pop,
    pub strata: &'a [T3Stratum],
}

/// Scores of every arm on the evaluation (or rehearsal) populations.
pub struct Scored {
    pub primary: PopScores,
    pub restricted: PopScores,
    pub alltext_sub: PopScores,
    pub t3: Vec<BTreeMap<char, T3Scores>>,
}

/// Scores every frozen arm once (v1's arms and, when fitted, arm F).
pub fn score_all(inp: &Inputs, ev: &EvalInputs, threads: usize) -> Result<Scored> {
    let tr = ev.trained;
    let trr = ev.trained_restricted;
    eprintln!("[{}] scoring T1/T2", ev.label);
    let primary = score_t12(inp, tr, ev.pop, threads)?;
    let restricted = score_t12(inp, trr, ev.pop_restricted, threads)?;
    let alltext_sub = score_t12(inp, tr, ev.pop_restricted, threads)?;
    eprintln!("[{}] scoring T3", ev.label);
    let mut t3: Vec<BTreeMap<char, T3Scores>> = Vec::new();
    for st in ev.strata {
        t3.push(score_t3_all(inp, tr, st, threads)?);
    }
    Ok(Scored {
        primary,
        restricted,
        alltext_sub,
        t3,
    })
}

/// The analysis to run. V1 is v1's evaluation exactly (arms A to E, v1's
/// contrasts and decision); V2 adds arm F, the F - D contrast and the v2
/// decision (PROTOCOL-v2.md sections 5-7).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Spec {
    V1,
    V2,
}

impl Spec {
    pub fn arms(self) -> &'static [char] {
        match self {
            Spec::V1 => &ARMS,
            Spec::V2 => &ARMS_V2,
        }
    }

    pub fn contrasts(self) -> &'static [(char, char)] {
        match self {
            Spec::V1 => &CONTRASTS,
            Spec::V2 => &CONTRASTS_V2,
        }
    }
}

/// Writes the per-pair scores (with `out` None, hashes exactly the bytes
/// that would be written), bootstraps and applies the decision rule.
/// Returns the results document.
pub fn analyze(
    inp: &Inputs,
    ev: &EvalInputs,
    scored: &Scored,
    spec: Spec,
    out: Option<&Path>,
    threads: usize,
) -> Result<Value> {
    let tr = ev.trained;
    let trr = ev.trained_restricted;
    let arms = spec.arms();
    ensure!(
        spec == Spec::V1 || tr.fusion_f.is_some(),
        "the v2 analysis needs a fitted arm F"
    );
    let (primary, restricted, alltext_sub, t3) =
        (&scored.primary, &scored.restricted, &scored.alltext_sub, &scored.t3);

    let mut files = Map::new();
    let (n, sha) = write_t12_pairs(
        inp,
        PairSink::open(out, "pairs-t12-primary.jsonl")?,
        ev.pop,
        &[("all_text", primary)],
        arms,
    )?;
    files.insert("pairs-t12-primary.jsonl".into(), json!({"rows":n,"sha256":sha}));
    let (n, sha) = write_t12_pairs(
        inp,
        PairSink::open(out, "pairs-t12-coverage-restricted.jsonl")?,
        ev.pop_restricted,
        &[("coverage_restricted", restricted), ("all_text_same_subset", alltext_sub)],
        arms,
    )?;
    files.insert("pairs-t12-coverage-restricted.jsonl".into(), json!({"rows":n,"sha256":sha}));
    for (st, sc) in ev.strata.iter().zip(t3) {
        let name = format!("pairs-t3-{}.jsonl", st.name);
        let (n, sha) = write_t3_pairs(inp, PairSink::open(out, &name)?, st, sc, arms)?;
        files.insert(name, json!({"rows":n,"sha256":sha}));
    }

    // Blocks.
    let mut reg = Registry::default();
    let mut blocks = Vec::new();
    t1_blocks(&mut reg, &mut blocks, "primary", Units::Primary, inp, ev.pop, primary, arms);
    t1_blocks(&mut reg, &mut blocks, "coverage_restricted", Units::Restricted, inp, ev.pop_restricted, restricted, arms);
    t1_blocks(&mut reg, &mut blocks, "all_text_on_restricted_subset", Units::Restricted, inp, ev.pop_restricted, alltext_sub, arms);
    t2_blocks(&mut reg, &mut blocks, tr, ev.pop, primary, arms);
    for (si, (st, sc)) in ev.strata.iter().zip(t3).enumerate() {
        for &arm in arms {
            for (name, vals) in t3_target_metrics(st, &sc[&arm]) {
                if vals.iter().all(|v| !v.is_finite()) {
                    continue;
                }
                blocks.push(Block::Mean {
                    units: Units::T3(si),
                    vals,
                    slot: reg.slot(&format!("T3/{}/{name}", st.name), arm),
                });
            }
        }
    }
    let nslots = reg.names.len();

    // Replicate multiplicities: per replicate, primary domains in order, then
    // T3 strata in order, from one ChaCha20 stream.
    let seed: [u8; 32] = {
        let h = sha_hex(SEED_STRING.as_bytes());
        hex::decode(h)?.try_into().map_err(|_| anyhow::anyhow!("seed"))?
    };
    let mut rng = ChaCha20::from_seed(seed);
    let restricted_map: Vec<usize> = ev
        .pop_restricted
        .authors
        .iter()
        .map(|a| {
            ev.pop
                .authors
                .iter()
                .position(|b| b.writer == a.writer && b.domain == a.domain)
                .context("restricted author absent from the primary population")
        })
        .collect::<Result<_>>()?;
    let mut mults: Vec<Vec<Vec<f64>>> = Vec::with_capacity(REPLICATES);
    for _ in 0..REPLICATES {
        let mut prim = vec![0.0; ev.pop.authors.len()];
        for d in 0..ev.pop.domains.len() {
            let list = &ev.pop.dom_authors[d];
            for _ in 0..list.len() {
                prim[list[rng.below(list.len() as u32) as usize]] += 1.0;
            }
        }
        let restr: Vec<f64> = restricted_map.iter().map(|&i| prim[i]).collect();
        let mut v = vec![prim, restr];
        for st in ev.strata {
            let mut m = vec![0.0; st.targets.len()];
            for _ in 0..st.targets.len() {
                m[rng.below(st.targets.len() as u32) as usize] += 1.0;
            }
            v.push(m);
        }
        mults.push(v);
    }

    let mut point = vec![f64::NAN; nslots];
    for b in &blocks {
        eval_block(b, None, &mut point);
    }
    eprintln!("[{}] bootstrap: {} blocks, {} slots", ev.label, blocks.len(), nslots);
    let reps: Vec<Vec<f64>> = par_map(REPLICATES, threads, |r| {
        let mut o = vec![f64::NAN; nslots];
        for b in &blocks {
            eval_block(b, Some(&mults[r]), &mut o);
        }
        o
    });

    // Derived: domain-macro T1 AUC per population, k and arm.
    let mut names: Vec<(String, char)> = reg.names.clone();
    let mut point_all = point.clone();
    let mut reps_all: Vec<Vec<f64>> = (0..nslots).map(|i| reps.iter().map(|r| r[i]).collect()).collect();
    for (label, pop) in [
        ("primary", ev.pop),
        ("coverage_restricted", ev.pop_restricted),
        ("all_text_on_restricted_subset", ev.pop_restricted),
    ] {
        for &arm in arms {
            for k in KS {
                let idx: Vec<usize> = pop
                    .domains
                    .iter()
                    .filter_map(|d| reg.index.get(&(format!("{label}/T1/domain:{d}/k{k}/auc"), arm)).copied())
                    .collect();
                let mac = |vals: &dyn Fn(usize) -> f64| -> f64 {
                    let v: Vec<f64> = idx.iter().map(|&i| vals(i)).filter(|x| x.is_finite()).collect();
                    if v.is_empty() { f64::NAN } else { mean(&v) }
                };
                point_all.push(mac(&|i| point[i]));
                reps_all.push((0..REPLICATES).map(|r| mac(&|i| reps[r][i])).collect());
                names.push((format!("{label}/T1/domain_macro/k{k}/auc"), arm));
            }
        }
    }
    let lookup: HashMap<(String, char), usize> = names.iter().cloned().enumerate().map(|(i, k)| (k, i)).collect();

    // Metrics and contrasts.
    let mut metric_names: Vec<String> = Vec::new();
    for (n, _) in &names {
        if !metric_names.contains(n) {
            metric_names.push(n.clone());
        }
    }
    let mut metrics = Map::new();
    let mut contrasts = Map::new();
    for name in &metric_names {
        let mut per = Map::new();
        for &arm in arms {
            if let Some(&i) = lookup.get(&(name.clone(), arm)) {
                per.insert(s(arm), summary_json(point_all[i], &reps_all[i], false));
            }
        }
        metrics.insert(name.clone(), Value::Object(per));
        let mut cs = Map::new();
        for &(x, y) in spec.contrasts() {
            if let (Some(&i), Some(&j)) = (lookup.get(&(name.clone(), x)), lookup.get(&(name.clone(), y))) {
                let d: Vec<f64> = reps_all[i].iter().zip(&reps_all[j]).map(|(a, b)| a - b).collect();
                cs.insert(format!("{x}-{y}"), summary_json(point_all[i] - point_all[j], &d, true));
            }
        }
        contrasts.insert(name.clone(), Value::Object(cs));
    }
    // Coverage cost per arm: primary minus restricted (and same-subset all text
    // minus restricted).
    let mut coverage = Map::new();
    for &arm in arms {
        let mut per = Map::new();
        for k in KS {
            for scope in ["pooled", "domain_macro"] {
                // Absent only for an arm F whose restricted refit failed.
                let Some(&r) = lookup.get(&(format!("coverage_restricted/T1/{scope}/k{k}/auc"), arm)) else {
                    continue;
                };
                for (label, from) in [
                    ("primary_minus_restricted", format!("primary/T1/{scope}/k{k}/auc")),
                    ("same_subset_all_text_minus_restricted", format!("all_text_on_restricted_subset/T1/{scope}/k{k}/auc")),
                ] {
                    let p = lookup[&(from, arm)];
                    let d: Vec<f64> = reps_all[p].iter().zip(&reps_all[r]).map(|(a, b)| a - b).collect();
                    per.insert(format!("{label}/{scope}/k{k}"), summary_json(point_all[p] - point_all[r], &d, false));
                }
            }
        }
        coverage.insert(s(arm), Value::Object(per));
    }

    let decision = decision_v1(&contrasts, ev);
    let mut t3_pops = Map::new();
    for st in ev.strata {
        t3_pops.insert(
            st.name.clone(),
            json!({"targets":st.targets.len(),"records":st.records.len(),
                "full":st.records.iter().filter(|r| r.kind=="full").count(),
                "mirror":st.records.iter().filter(|r| r.kind=="mirror").count(),
                "span":st.records.iter().filter(|r| r.kind=="span").count(),
                "own_sources":st.targets.iter().map(|t| t.sources.len()).sum::<usize>(),
                "targets_with_train_split_documents":st.targets.iter().filter(|t| t.has_train).count()}),
        );
    }
    let mut res = json!({
        "populations":{"t12_primary":pop_summary(ev.pop),"t12_coverage_restricted":pop_summary(ev.pop_restricted),"t3":t3_pops},
        "files":files,
        "bootstrap":{"replicates":REPLICATES,"seed_string":SEED_STRING,"rng":"ChaCha20 (20 rounds, 64-bit counter, stream 0) keyed by sha256(seed_string); uniform index by multiply-shift with rejection",
            "order":"per replicate: primary T1/T2 domains in name order (n_d draws each), then T3 strata in protocol order (n_targets draws each); restricted-subset authors take their primary multiplicity",
            "interval":"percentile, type 7"},
        "decision":decision.clone(),
        "metrics":metrics,
        "contrasts":contrasts,
        "coverage_cost":coverage,
    });
    if spec == Spec::V2 {
        let d2 = decision_v2(&res["contrasts"]);
        res["decision"] = d2;
        res["decision_v1_rule_recomputed"] = decision;
        res["arm_F"] = json!({"score":"F = b0 + b_B z_B + b_C z_C + b_A z_A, z_X = (s_X - m_X,k) / d_X,k with v1's impostor constants",
            "primary":tr.record_v2.clone(),"coverage_restricted":trr.record_v2.clone()});
    }
    Ok(res)
}

/// PROTOCOL-v2.md section 6: CFG adds resolved value on top of the encoder
/// and stylometry only if the percentile 95% interval of
/// Delta = AUC_F - AUC_D (primary/T1/pooled/k5/auc) has lower bound > 0.
pub fn decision_v2(contrasts: &Value) -> Value {
    let d = contrasts[PRIMARY_METRIC]["F-D"].clone();
    let lo = d["ci95"][0].as_f64();
    let hi = d["ci95"][1].as_f64();
    let (outcome, position) = match (lo, hi) {
        (Some(lo), Some(_)) if lo > 0.0 => ("cfg_adds_resolved_value_beyond_d", "above_zero"),
        (Some(_), Some(hi)) if hi < 0.0 => ("no_resolved_cfg_contribution_beyond_encoder_plus_stylometry", "below_zero"),
        (Some(_), Some(_)) => ("no_resolved_cfg_contribution_beyond_encoder_plus_stylometry", "includes_zero"),
        _ => ("not_evaluable", "undefined"),
    };
    json!({"outcome":outcome,"interval_position":position,
        "rule":"cfg_adds_resolved_value_beyond_d iff the percentile 95% interval of Delta = AUC_F - AUC_D on primary/T1/pooled/k5/auc has lower bound strictly > 0; no margin; ci97_5 not used",
        "metric":PRIMARY_METRIC,"contrast":"F-D","delta_T1_F_D":d})
}

/// The v1 decision rule (PROTOCOL.md section 9), unchanged.
fn decision_v1(contrasts: &Map<String, Value>, ev: &EvalInputs) -> Value {
    let get = |name: &str, c: &str| -> Value { contrasts[name][c].clone() };
    let above = |v: &Value| v["ci95"][0].as_f64().is_some_and(|lo| lo > 0.0);
    let t1_ab = get("primary/T1/pooled/k5/auc", "A-B");
    let t1_eb = get("primary/T1/pooled/k5/auc", "E-B");
    let primary_targets = ev.strata.iter().find(|s| s.name == "primary" || s.name == "rehearsal").map_or(0, |s| s.targets.len());
    let t3_name = format!("T3/{}/auc", ev.strata[0].name);
    let t3_ab = get(&t3_name, "A-B");
    let t3_eb = get(&t3_name, "E-B");
    let t3_ok = primary_targets >= 30;
    let rule1 = t3_ok && above(&t1_ab) && above(&t3_ab);
    let rule2 = !rule1 && (above(&t1_eb) || (t3_ok && above(&t3_eb)));
    let outcome = if rule1 {
        "cfg_wins_clearly"
    } else if rule2 {
        "cfg_adds_value"
    } else {
        "no_evidence_for_cfg"
    };
    json!({
        "outcome":outcome,
        "t3_primary_targets":primary_targets,"rule_1_available":t3_ok,
        "rule_1":{"delta_T1_A_B":t1_ab,"delta_T3_A_B":t3_ab,"satisfied":rule1},
        "rule_2":{"delta_T1_E_B":t1_eb,"delta_T3_E_B":t3_eb,"satisfied":rule2},
        "context":{"T1":{"D-B":get("primary/T1/pooled/k5/auc","D-B"),"C-B":get("primary/T1/pooled/k5/auc","C-B")},
            "T3":{"D-B":get(&t3_name,"D-B"),"C-B":get(&t3_name,"C-B")}},
    })
}
