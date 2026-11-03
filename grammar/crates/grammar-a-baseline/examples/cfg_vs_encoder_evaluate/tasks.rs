//! Populations (T1/T2 authors, T3 strata), per-domain score matrices, and the
//! T1 trials, T2 identification records and T3 per-target statistics derived
//! from them.
use crate::common::sha_hex;
use crate::load::*;
use crate::space::*;
use crate::stats::Item;
use anyhow::{Context, Result, ensure};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::atomic::{AtomicUsize, Ordering};

pub const KS: [usize; 3] = [1, 3, 5];
pub const MAX_IMPOSTORS: usize = 100;
pub const LENGTH_STRATA: [(u32, u32, &str); 3] =
    [(150, 249, "150-249"), (250, 349, "250-349"), (350, 450, "350-450")];

/// Parallel map over 0..n with `threads` workers, results in index order.
pub fn par_map<T: Send, F: Fn(usize) -> T + Sync>(n: usize, threads: usize, f: F) -> Vec<T> {
    let next = AtomicUsize::new(0);
    let mut parts: Vec<Vec<(usize, T)>> = Vec::new();
    std::thread::scope(|s| {
        let handles: Vec<_> = (0..threads.max(1))
            .map(|_| {
                s.spawn(|| {
                    let mut out = Vec::new();
                    loop {
                        let i = next.fetch_add(1, Ordering::Relaxed);
                        if i >= n {
                            break;
                        }
                        out.push((i, f(i)));
                    }
                    out
                })
            })
            .collect();
        for h in handles {
            parts.push(h.join().expect("worker panicked"));
        }
    });
    let mut all: Vec<(usize, T)> = parts.into_iter().flatten().collect();
    all.sort_by_key(|p| p.0);
    all.into_iter().map(|p| p.1).collect()
}

#[derive(Clone, Debug)]
pub struct PAuthor {
    pub domain: usize,
    pub writer: String,
    pub h: String,
    pub fold: u32,
    /// Withheld from T2 galleries (eval: withheld; training: simulated).
    pub withheld: bool,
    pub queries: Vec<usize>,
    pub refs: Vec<usize>,
}

#[derive(Clone, Debug, Default)]
pub struct Pop {
    pub domains: Vec<String>,
    pub authors: Vec<PAuthor>,
    pub dom_authors: Vec<Vec<usize>>,
    /// (author, document) per query.
    pub queries: Vec<(usize, usize)>,
    pub dom_queries: Vec<Vec<usize>>,
    pub author_local: Vec<usize>,
    pub query_local: Vec<usize>,
}

impl Pop {
    pub fn build(mut authors: Vec<PAuthor>, domains: Vec<String>) -> Pop {
        authors.sort_by(|a, b| (a.domain, &a.h).cmp(&(b.domain, &b.h)));
        let nd = domains.len();
        let mut dom_authors = vec![Vec::new(); nd];
        let mut author_local = vec![0; authors.len()];
        for (i, a) in authors.iter().enumerate() {
            author_local[i] = dom_authors[a.domain].len();
            dom_authors[a.domain].push(i);
        }
        let mut queries = Vec::new();
        let mut dom_queries = vec![Vec::new(); nd];
        let mut query_local = Vec::new();
        for (i, a) in authors.iter().enumerate() {
            for &q in &a.queries {
                query_local.push(dom_queries[a.domain].len());
                dom_queries[a.domain].push(queries.len());
                queries.push((i, q));
            }
        }
        Pop {
            domains,
            authors,
            dom_authors,
            queries,
            dom_queries,
            author_local,
            query_local,
        }
    }

    /// Authors restricted to a subset (e.g. one training fold), same domains.
    pub fn subset(&self, keep: impl Fn(&PAuthor) -> bool) -> Pop {
        Pop::build(
            self.authors.iter().filter(|a| keep(a)).cloned().collect(),
            self.domains.clone(),
        )
    }

    /// Documents with author group ids, for fitting statistics.
    pub fn training_docs(&self) -> (Vec<usize>, Vec<usize>) {
        let mut docs = Vec::new();
        let mut groups = Vec::new();
        for (i, a) in self.authors.iter().enumerate() {
            for &d in a.queries.iter().chain(&a.refs) {
                docs.push(d);
                groups.push(i);
            }
        }
        (docs, groups)
    }

    pub fn all_docs(&self) -> BTreeSet<usize> {
        self.authors
            .iter()
            .flat_map(|a| a.queries.iter().chain(&a.refs).copied())
            .collect()
    }
}

/// T1/T2 population of one role. With `restricted`, an author qualifies when
/// all 5 references and at least one query have restricted text; only
/// qualifying queries are kept. Authors with an execution failure in any
/// selected document are excluded and counted.
pub fn t12_pop(inp: &Inputs, role: &str, restricted: bool) -> Result<(Pop, usize, usize)> {
    let mut domains: BTreeSet<String> = BTreeSet::new();
    for a in &inp.authors {
        if a.selected && a.role == role {
            domains.insert(a.domain.clone());
        }
    }
    let domains: Vec<String> = domains.into_iter().collect();
    let mut out = Vec::new();
    let mut failures = 0usize;
    let mut unqualified = 0usize;
    for a in &inp.authors {
        if !(a.selected && a.role == role) {
            continue;
        }
        let queries: Vec<usize> = a.queries.iter().map(|q| inp.doc(q)).collect::<Result<_>>()?;
        let refs: Vec<usize> = a.refs.iter().map(|q| inp.doc(q)).collect::<Result<_>>()?;
        ensure!(refs.len() == 5 && !queries.is_empty(), "selected author shape");
        for &d in queries.iter().chain(&refs) {
            ensure!(
                inp.docs[d].writer.as_deref() == Some(a.writer.as_str())
                    && inp.docs[d].domain == a.domain,
                "document writer or domain differs from author"
            );
        }
        if queries
            .iter()
            .chain(&refs)
            .any(|&d| inp.cfg[d].as_ref().is_none_or(|c| c.execution_failures > 0))
        {
            failures += 1;
            continue;
        }
        let queries: Vec<usize> = if restricted {
            if !refs.iter().all(|&d| inp.restricted[d]) {
                unqualified += 1;
                continue;
            }
            let q: Vec<usize> = queries.into_iter().filter(|&d| inp.restricted[d]).collect();
            if q.is_empty() {
                unqualified += 1;
                continue;
            }
            q
        } else {
            queries
        };
        let withheld = if role == "eval" {
            a.withheld
        } else {
            a.simulated_withheld
        };
        ensure!(withheld == (a.fold == 0), "withheld rule");
        out.push(PAuthor {
            domain: domains.binary_search(&a.domain).unwrap(),
            writer: a.writer.clone(),
            h: a.h.clone(),
            fold: a.fold,
            withheld,
            queries,
            refs,
        });
    }
    Ok((Pop::build(out, domains), failures, unqualified))
}

/// Impostor authors per query: all other same-domain authors when at most
/// 100, else the 100 with the smallest sha256(query_id TAB writer_key).
pub fn impostor_sets(inp: &Inputs, pop: &Pop) -> Vec<Vec<usize>> {
    pop.queries
        .iter()
        .map(|&(a, doc)| {
            let d = pop.authors[a].domain;
            let mut others: Vec<usize> = pop.dom_authors[d].iter().copied().filter(|&b| b != a).collect();
            if others.len() > MAX_IMPOSTORS {
                let qid = &inp.docs[doc].id;
                let mut keyed: Vec<(String, usize)> = others
                    .iter()
                    .map(|&b| (sha_hex(format!("{qid}\t{}", pop.authors[b].writer).as_bytes()), b))
                    .collect();
                keyed.sort();
                others = keyed.into_iter().take(MAX_IMPOSTORS).map(|k| k.1).collect();
            }
            others
        })
        .collect()
}

/// Scores of every same-domain (query, author profile) pair at each k.
#[derive(Clone, Debug)]
pub struct Mat {
    /// [domain][query_local * n_authors + author_local]
    pub per_dom: Vec<Vec<f64>>,
}

impl Mat {
    pub fn get(&self, pop: &Pop, q: usize, a: usize) -> f64 {
        let d = pop.authors[pop.queries[q].0].domain;
        let na = pop.dom_authors[d].len();
        self.per_dom[d][pop.query_local[q] * na + pop.author_local[a]]
    }

    pub fn map2(a: &Mat, b: &Mat, f: impl Fn(f64, f64) -> f64) -> Mat {
        Mat {
            per_dom: a
                .per_dom
                .iter()
                .zip(&b.per_dom)
                .map(|(x, y)| x.iter().zip(y).map(|(p, q)| f(*p, *q)).collect())
                .collect(),
        }
    }
}

pub fn prepare_docs(
    inp: &Inputs,
    model: &Model,
    docs: &BTreeSet<usize>,
    threads: usize,
) -> Result<HashMap<usize, Prepared>> {
    let list: Vec<usize> = docs.iter().copied().collect();
    let out: Vec<Result<Prepared>> = par_map(list.len(), threads, |i| {
        let r = raw(inp, list[i], model.config.rep, model.text)?;
        Ok(model.prepare(&r))
    });
    let mut map = HashMap::new();
    for (d, p) in list.into_iter().zip(out) {
        let p = p?;
        ensure!(p.n.is_finite() && p.n > 0.0, "degenerate standardized document");
        map.insert(d, p);
    }
    Ok(map)
}

/// Score matrices at each k in `ks` for one fitted model.
pub fn score_pop(
    model: &Model,
    pop: &Pop,
    prep: &HashMap<usize, Prepared>,
    ks: &[usize],
    threads: usize,
) -> Vec<Mat> {
    let jobs: Vec<(usize, usize)> = (0..pop.domains.len())
        .flat_map(|d| (0..pop.dom_authors[d].len()).map(move |al| (d, al)))
        .collect();
    let cols: Vec<Vec<Vec<f64>>> = par_map(jobs.len(), threads, |j| {
        let (d, al) = jobs[j];
        let a = &pop.authors[pop.dom_authors[d][al]];
        ks.iter()
            .map(|&k| {
                let refs: Vec<&Prepared> = a.refs[..k].iter().map(|r| &prep[r]).collect();
                let prof = profile(model, &refs);
                pop.dom_queries[d]
                    .iter()
                    .map(|&q| score(&prof, &prep[&pop.queries[q].1]))
                    .collect()
            })
            .collect()
    });
    let mut mats: Vec<Mat> = ks
        .iter()
        .map(|_| Mat {
            per_dom: (0..pop.domains.len())
                .map(|d| vec![f64::NAN; pop.dom_queries[d].len() * pop.dom_authors[d].len()])
                .collect(),
        })
        .collect();
    for (j, col) in cols.into_iter().enumerate() {
        let (d, al) = jobs[j];
        let na = pop.dom_authors[d].len();
        for (ki, scores) in col.into_iter().enumerate() {
            for (ql, s) in scores.into_iter().enumerate() {
                mats[ki].per_dom[d][ql * na + al] = s;
            }
        }
    }
    mats
}

/// T1 trials in a fixed order (author, query, genuine then impostors), so the
/// trials of two arms on the same population align element-wise. `keep`
/// filters queries (strata); weights follow the protocol: each author's
/// genuine trials total 1 and so do its impostor trials.
pub fn t1_items(
    pop: &Pop,
    mat: &Mat,
    imps: &[Vec<usize>],
    keep: &dyn Fn(usize) -> bool,
) -> Vec<Item> {
    let mut by_author: Vec<Vec<usize>> = vec![Vec::new(); pop.authors.len()];
    for (q, &(a, _)) in pop.queries.iter().enumerate() {
        if keep(q) {
            by_author[a].push(q);
        }
    }
    let mut out = Vec::new();
    for (a, qs) in by_author.iter().enumerate() {
        if qs.is_empty() {
            continue;
        }
        let nq = qs.len() as f64;
        for &q in qs {
            out.push(Item {
                s: mat.get(pop, q, a),
                pos: true,
                unit: a as u32,
                w: 1.0 / nq,
            });
            let ni = imps[q].len() as f64;
            for &b in &imps[q] {
                out.push(Item {
                    s: mat.get(pop, q, b),
                    pos: false,
                    unit: a as u32,
                    w: 1.0 / (nq * ni),
                });
            }
        }
    }
    out
}

/// T2 per-query outcome against the enrolled (non-withheld) same-domain gallery.
#[derive(Clone, Copy, Debug)]
pub struct T2Query {
    pub author: usize,
    pub enrolled: bool,
    pub top1: bool,
    pub rr: f64,
    pub max: f64,
}

pub fn t2_queries(pop: &Pop, mat: &Mat) -> Vec<T2Query> {
    pop.queries
        .iter()
        .enumerate()
        .filter_map(|(q, &(a, _))| {
            let d = pop.authors[a].domain;
            let gallery: Vec<usize> = pop.dom_authors[d]
                .iter()
                .copied()
                .filter(|&b| !pop.authors[b].withheld)
                .collect();
            if gallery.is_empty() {
                return None;
            }
            let max = gallery
                .iter()
                .map(|&b| mat.get(pop, q, b))
                .fold(f64::NEG_INFINITY, f64::max);
            let enrolled = !pop.authors[a].withheld;
            let (top1, rr) = if enrolled {
                let own = mat.get(pop, q, a);
                let ahead = gallery
                    .iter()
                    .filter(|&&b| b != a && mat.get(pop, q, b) >= own)
                    .count();
                (ahead == 0, 1.0 / (1 + ahead) as f64)
            } else {
                (false, 0.0)
            };
            Some(T2Query {
                author: a,
                enrolled,
                top1,
                rr,
                max,
            })
        })
        .collect()
}

/// Open-set threshold: the type-7 90th percentile of the maximum gallery
/// scores of withheld queries (10% of withheld queries falsely accepted).
pub fn t2_threshold(qs: &[T2Query]) -> f64 {
    let mut v: Vec<f64> = qs.iter().filter(|q| !q.enrolled).map(|q| q.max).collect();
    v.sort_by(f64::total_cmp);
    crate::stats::quantile(&v, 0.90)
}

// ---------------------------------------------------------------- T3

#[derive(Clone, Debug)]
pub struct T3Target {
    pub writer: String,
    pub profile: Vec<usize>,
    pub has_train: bool,
    pub sources: Vec<usize>,
}

#[derive(Clone, Debug)]
pub struct T3Rec {
    pub doc: usize,
    pub target: usize,
    pub kind: String,
    pub family: String,
    pub parent_writer: Option<String>,
    pub source: Option<usize>,
}

#[derive(Clone, Debug)]
pub struct T3Stratum {
    pub name: String,
    pub targets: Vec<T3Target>,
    pub records: Vec<T3Rec>,
}

impl T3Stratum {
    pub fn docs(&self) -> BTreeSet<usize> {
        let mut s = BTreeSet::new();
        for t in &self.targets {
            s.extend(t.profile.iter().copied());
            s.extend(t.sources.iter().copied());
        }
        for r in &self.records {
            s.insert(r.doc);
        }
        s
    }
}

pub const T3_ORDER: [&str; 4] = ["primary", "bundled_train", "dev_iter_local", "frontier_span"];

pub fn t3_strata(inp: &Inputs) -> Result<Vec<T3Stratum>> {
    let mut out = Vec::new();
    for name in T3_ORDER {
        let mut targets: Vec<T3Target> = Vec::new();
        let mut tindex: HashMap<String, usize> = HashMap::new();
        let mut rows: Vec<&TargetRow> = inp
            .targets
            .iter()
            .filter(|t| t.stratum == name && t.eligible)
            .collect();
        rows.sort_by(|a, b| a.target.cmp(&b.target));
        for t in rows {
            ensure!(t.profile.len() == 5, "T3 profile size");
            for p in &t.profile {
                ensure!(inp.docs[inp.doc(p)?].kind == "human_document", "T3 profile document kind");
            }
            tindex.insert(t.target.clone(), targets.len());
            targets.push(T3Target {
                writer: t.target.clone(),
                profile: t.profile.iter().map(|p| inp.doc(p)).collect::<Result<_>>()?,
                has_train: t.has_train,
                sources: Vec::new(),
            });
        }
        let mut records = Vec::new();
        let mut recs: Vec<&RecordRow> = inp.records.iter().filter(|r| r.stratum == name).collect();
        recs.sort_by(|a, b| a.id.cmp(&b.id));
        for r in recs {
            let t = *tindex.get(&r.target).context("T3 record target not eligible")?;
            ensure!(
                r.parent_writer.as_deref() != Some(r.target.as_str()),
                "T3 record parent by its own target"
            );
            let source = if r.kind == "full" && r.source_available {
                let p = inp.doc(r.parent_id.as_deref().context("full record parent")?)?;
                ensure!(
                    inp.docs[p].writer == r.parent_writer,
                    "source writer differs from record parent writer"
                );
                Some(p)
            } else {
                None
            };
            if let Some(p) = source
                && !targets[t].sources.contains(&p)
            {
                targets[t].sources.push(p);
            }
            let doc = inp.doc(&r.id)?;
            ensure!(inp.docs[doc].kind == "t3_record", "T3 record document kind");
            if let Some(p) = source {
                ensure!(inp.docs[p].kind == "human_document", "T3 source document kind");
            }
            records.push(T3Rec {
                doc,
                target: t,
                kind: r.kind.clone(),
                family: r.family.clone(),
                parent_writer: r.parent_writer.clone(),
                source,
            });
        }
        for t in &mut targets {
            t.sources.sort_unstable();
        }
        out.push(T3Stratum {
            name: name.to_string(),
            targets,
            records,
        });
    }
    Ok(out)
}

/// Per target: scores of every record of the stratum and of its own sources
/// under the k = 5 profile.
#[derive(Clone, Debug)]
pub struct T3Scores {
    pub rec: Vec<Vec<f64>>,
    pub src: Vec<Vec<f64>>,
}

impl T3Scores {
    pub fn map2(a: &T3Scores, b: &T3Scores, f: impl Fn(f64, f64) -> f64) -> T3Scores {
        let m = |x: &Vec<Vec<f64>>, y: &Vec<Vec<f64>>| -> Vec<Vec<f64>> {
            x.iter()
                .zip(y)
                .map(|(p, q)| p.iter().zip(q).map(|(s, t)| f(*s, *t)).collect())
                .collect()
        };
        T3Scores {
            rec: m(&a.rec, &b.rec),
            src: m(&a.src, &b.src),
        }
    }
}

pub fn score_t3(
    model: &Model,
    st: &T3Stratum,
    prep: &HashMap<usize, Prepared>,
    threads: usize,
) -> T3Scores {
    let per: Vec<(Vec<f64>, Vec<f64>)> = par_map(st.targets.len(), threads, |t| {
        let tg = &st.targets[t];
        let refs: Vec<&Prepared> = tg.profile.iter().map(|r| &prep[r]).collect();
        let prof = profile(model, &refs);
        (
            st.records.iter().map(|r| score(&prof, &prep[&r.doc])).collect(),
            tg.sources.iter().map(|s| score(&prof, &prep[s])).collect(),
        )
    });
    let (rec, src) = per.into_iter().unzip();
    T3Scores { rec, src }
}

fn pair_auc(pos: &[f64], neg: &[f64]) -> f64 {
    if pos.is_empty() || neg.is_empty() {
        return f64::NAN;
    }
    let mut n: Vec<f64> = neg.to_vec();
    n.sort_by(f64::total_cmp);
    let mut acc = 0.0;
    for &p in pos {
        let below = n.partition_point(|&x| x < p);
        let upto = n.partition_point(|&x| x <= p);
        acc += below as f64 + 0.5 * (upto - below) as f64;
    }
    acc / (pos.len() as f64 * n.len() as f64)
}

/// Per-target T3 statistics (NaN where undefined for that target).
pub fn t3_target_metrics(st: &T3Stratum, sc: &T3Scores) -> BTreeMap<String, Vec<f64>> {
    let mut kinds: BTreeSet<&str> = BTreeSet::new();
    let mut fams: BTreeSet<&str> = BTreeSet::new();
    for r in &st.records {
        kinds.insert(&r.kind);
        fams.insert(&r.family);
    }
    let mut out: BTreeMap<String, Vec<f64>> = BTreeMap::new();
    let mut push = |k: String, v: f64| out.entry(k).or_default().push(v);
    for (t, tg) in st.targets.iter().enumerate() {
        let s = &sc.rec[t];
        let other: Vec<f64> = st
            .records
            .iter()
            .enumerate()
            .filter(|(_, r)| r.target != t && r.parent_writer.as_deref() != Some(tg.writer.as_str()))
            .map(|(i, _)| s[i])
            .collect();
        let mut neg = other.clone();
        neg.extend(sc.src[t].iter().copied());
        let pos_of = |pred: &dyn Fn(&T3Rec) -> bool| -> Vec<f64> {
            st.records
                .iter()
                .enumerate()
                .filter(|(_, r)| r.target == t && pred(r))
                .map(|(i, _)| s[i])
                .collect()
        };
        let pos = pos_of(&|_| true);
        push("auc".into(), pair_auc(&pos, &neg));
        push("auc_other_targets_only".into(), pair_auc(&pos, &other));
        for k in &kinds {
            push(format!("auc_kind:{k}"), pair_auc(&pos_of(&|r| r.kind == *k), &neg));
        }
        for f in &fams {
            push(format!("auc_family:{f}"), pair_auc(&pos_of(&|r| r.family == *f), &neg));
        }
        // T3b: own source only, full rewrites.
        let mut wins = Vec::new();
        for (i, r) in st.records.iter().enumerate() {
            if r.target == t
                && let Some(src) = r.source
            {
                let j = tg.sources.binary_search(&src).unwrap();
                let (a, b) = (s[i], sc.src[t][j]);
                wins.push(if a > b { 1.0 } else if a == b { 0.5 } else { 0.0 });
            }
        }
        push(
            "own_source_win_share".into(),
            if wins.is_empty() {
                f64::NAN
            } else {
                wins.iter().sum::<f64>() / wins.len() as f64
            },
        );
    }
    // Train-split strata are expressed as target subsets of "auc".
    let auc = out["auc"].clone();
    for (key, flag) in [("auc_targets_with_train_split:true", true), ("auc_targets_with_train_split:false", false)] {
        out.insert(
            key.into(),
            st.targets
                .iter()
                .zip(&auc)
                .map(|(tg, v)| if tg.has_train == flag { *v } else { f64::NAN })
                .collect(),
        );
    }
    out
}
