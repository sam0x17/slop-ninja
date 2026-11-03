//! Arm representations, training-author vocabularies and standardization, and
//! the shared pooling contract
//! `slop_ninja_reference_normalized_mean_cosine_v1`.
//!
//! Every representation is a sparse vector x over feature IDs (absent = 0).
//! Standardized coordinates are z = (x - mean) / scale. Writing u = x / scale
//! (sparse) and v = mean / scale (dense), z = u - v, so norms, profiles and
//! cosines are computed without densifying documents.
use crate::load::*;
use anyhow::{Result, ensure};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::HashMap;

pub const SD_FLOOR: f64 = 0.01;
pub const CHAR3_VOCAB: usize = 2000;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum CfgSet {
    LeafGrammar,
    Structure,
    All,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum EncField {
    HRaw,
    ZShared,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Rep {
    Cfg { set: CfgSet, merged: bool },
    Enc(EncField),
    Stylo,
}

impl Rep {
    pub fn name(self) -> &'static str {
        match self {
            Rep::Cfg { set: CfgSet::LeafGrammar, merged: false } => "leaf_grammar",
            Rep::Cfg { set: CfgSet::Structure, merged: false } => "structure",
            Rep::Cfg { set: CfgSet::All, merged: false } => "all",
            Rep::Cfg { set: CfgSet::LeafGrammar, merged: true } => "leaf_grammar_pool_merged_v1",
            Rep::Cfg { set: CfgSet::All, merged: true } => "all_pool_merged_v1",
            Rep::Cfg { set: CfgSet::Structure, merged: true } => "structure_pool_merged_unlisted",
            Rep::Enc(EncField::HRaw) => "h_raw",
            Rep::Enc(EncField::ZShared) => "z_shared",
            Rep::Stylo => "function_words_plus_char3",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Scorer {
    S1,
    S2,
}

impl Scorer {
    pub fn name(self) -> &'static str {
        match self {
            Scorer::S1 => "S1",
            Scorer::S2 => "S2",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Config {
    pub rep: Rep,
    pub scorer: Scorer,
}

impl Config {
    pub fn name(&self) -> String {
        format!("{}+{}", self.rep.name(), self.scorer.name())
    }
}

/// Frozen candidate order per arm: candidate order, then S1 before S2.
pub fn candidates(arm: char) -> Vec<Config> {
    let reps: Vec<Rep> = match arm {
        'A' => vec![
            Rep::Cfg { set: CfgSet::LeafGrammar, merged: false },
            Rep::Cfg { set: CfgSet::Structure, merged: false },
            Rep::Cfg { set: CfgSet::All, merged: false },
            Rep::Cfg { set: CfgSet::LeafGrammar, merged: true },
            Rep::Cfg { set: CfgSet::All, merged: true },
        ],
        'B' => vec![Rep::Enc(EncField::HRaw), Rep::Enc(EncField::ZShared)],
        'C' => vec![Rep::Stylo],
        _ => vec![],
    };
    reps.into_iter()
        .flat_map(|rep| [Scorer::S1, Scorer::S2].map(|scorer| Config { rep, scorer }))
        .collect()
}

pub fn reps(arm: char) -> Vec<Rep> {
    let mut out = Vec::new();
    for c in candidates(arm) {
        if !out.contains(&c.rep) {
            out.push(c.rep);
        }
    }
    out
}

/// Sparse raw representation: (feature id, value) sorted by id, values > 0 for
/// CFG and stylometric features; all 768 coordinates for the encoder.
pub type Sparse = Vec<(u32, f64)>;

fn fixed_base(inp: &Inputs) -> u32 {
    inp.keys.len() as u32
}

fn merged_base(inp: &Inputs) -> u32 {
    inp.keys.len() as u32 + 3
}

/// Size of the raw feature-id space of a representation.
pub fn id_space(inp: &Inputs, rep: Rep) -> usize {
    match rep {
        Rep::Cfg { .. } => inp.keys.len() + 3 + inp.n_merged,
        Rep::Enc(_) => ENC_DIM,
        Rep::Stylo => N_FW + inp.char3.len(),
    }
}

fn admit(kind: u8, set: CfgSet) -> bool {
    match set {
        CfgSet::LeafGrammar => kind == 2,
        CfgSet::Structure => kind != 2,
        CfgSet::All => true,
    }
}

/// Raw transformed features of document `doc` (FROZEN.json arms.A/B/C).
pub fn raw(inp: &Inputs, doc: usize, rep: Rep, text: Text) -> Result<Sparse> {
    match rep {
        Rep::Cfg { set, merged } => {
            let row = inp.cfg[doc].as_ref().expect("cfg row");
            let tokens = match text {
                Text::Full => row.tokens,
                Text::Restricted => row.complete,
            };
            let mut acc: Vec<(u32, f64, bool)> = Vec::with_capacity(row.keys.len() + 3);
            for (&k, &c) in row.keys.iter().zip(&row.vals) {
                if c <= 0.0 {
                    continue;
                }
                match inp.keys[k as usize] {
                    KeyKind::Pool { pool, kind, merged: m } => {
                        if !admit(kind, set) {
                            continue;
                        }
                        if text == Text::Restricted && !COMPLETE_POOLS.contains(&pool) {
                            continue;
                        }
                        let id = if merged && MERGED_POOLS.contains(&pool) {
                            merged_base(inp) + m
                        } else {
                            k
                        };
                        acc.push((id, c, false));
                    }
                    KeyKind::Avail { doc_scope } => {
                        if set == CfgSet::All && text == Text::Full {
                            acc.push((k, c, doc_scope));
                        }
                    }
                }
            }
            if set == CfgSet::All && text == Text::Full {
                for (j, flag) in [row.projection_available, row.search_exhaustive, row.tokens > 0.0]
                    .into_iter()
                    .enumerate()
                {
                    if flag {
                        acc.push((fixed_base(inp) + j as u32, 1.0, true));
                    }
                }
            }
            acc.sort_by_key(|a| a.0);
            let mut out: Sparse = Vec::with_capacity(acc.len());
            let mut i = 0;
            while i < acc.len() {
                let (id, mut c, doc_scope) = acc[i];
                let mut j = i + 1;
                while j < acc.len() && acc[j].0 == id {
                    c += acc[j].1;
                    j += 1;
                }
                let denom = if doc_scope { 1.0 } else { tokens };
                ensure!(denom > 0.0, "count without token opportunities");
                let x = (c / denom).ln_1p();
                ensure!(x.is_finite(), "nonfinite CFG value");
                out.push((id, x));
                i = j;
            }
            Ok(out)
        }
        Rep::Enc(field) => {
            let row = inp.enc[text.slot()][doc].as_ref().expect("encoder row");
            let v = match field {
                EncField::HRaw => &row.h_raw,
                EncField::ZShared => &row.z_shared,
            };
            Ok(v.iter().enumerate().map(|(i, &x)| (i as u32, x)).collect())
        }
        Rep::Stylo => {
            let row = inp.stylo[text.slot()][doc].as_ref().expect("stylometric row");
            let mut out = Vec::with_capacity(N_FW + row.c3.len());
            ensure!(row.words > 0, "document without word tokens");
            for (j, &c) in row.fw.iter().enumerate() {
                if c > 0 {
                    out.push((j as u32, (1000.0 * c as f64 / row.words as f64).ln_1p()));
                }
            }
            ensure!(row.c3_total > 0, "document without character 3-grams");
            for &(g, c) in &row.c3 {
                if c > 0 {
                    out.push((
                        N_FW as u32 + g,
                        (1000.0 * c as f64 / row.c3_total as f64).ln_1p(),
                    ));
                }
            }
            Ok(out)
        }
    }
}

/// Training-author vocabulary: feature ids kept, in id order (dimension = position).
pub fn fit_vocab(inp: &Inputs, rep: Rep, train: &[&Sparse]) -> Vec<u32> {
    match rep {
        Rep::Enc(_) => (0..ENC_DIM as u32).collect(),
        Rep::Cfg { .. } => {
            let mut freq: HashMap<u32, u32> = HashMap::new();
            for r in train {
                for &(id, x) in r.iter() {
                    if x > 0.0 {
                        *freq.entry(id).or_default() += 1;
                    }
                }
            }
            let mut ids: Vec<u32> = freq
                .into_iter()
                .filter(|&(_, n)| n >= 2)
                .map(|(id, _)| id)
                .collect();
            ids.sort_unstable();
            ids
        }
        Rep::Stylo => {
            let mut freq: HashMap<u32, u32> = HashMap::new();
            for r in train {
                for &(id, x) in r.iter() {
                    if id >= N_FW as u32 && x > 0.0 {
                        *freq.entry(id).or_default() += 1;
                    }
                }
            }
            let mut ranked: Vec<(u32, u32)> = freq.into_iter().collect();
            ranked.sort_by(|a, b| {
                b.1.cmp(&a.1).then_with(|| {
                    inp.char3[(a.0 - N_FW as u32) as usize]
                        .as_bytes()
                        .cmp(inp.char3[(b.0 - N_FW as u32) as usize].as_bytes())
                })
            });
            let mut ids: Vec<u32> = (0..N_FW as u32).collect();
            ids.extend(ranked.into_iter().take(CHAR3_VOCAB).map(|(id, _)| id));
            ids.sort_unstable();
            ids
        }
    }
}

/// Projects a raw vector onto a vocabulary: (dimension, value).
pub fn project(lookup: &[u32], r: &Sparse) -> Sparse {
    r.iter()
        .filter_map(|&(id, x)| {
            let d = lookup[id as usize];
            (d != u32::MAX).then_some((d, x))
        })
        .collect()
}

/// Fitted statistics for one representation on one set of training documents.
#[derive(Clone, Debug)]
pub struct Stats {
    pub vocab: Vec<u32>,
    pub lookup: Vec<u32>,
    pub mean: Vec<f64>,
    pub sd_total: Vec<f64>,
    pub sd_within: Vec<f64>,
}

/// Fits vocabulary, weighted mean, total SD and pooled within-author SD. Each
/// document is weighted 1 / (its author's document count): `groups[i]` is the
/// author of training document i.
pub fn fit_stats(inp: &Inputs, rep: Rep, train: &[&Sparse], groups: &[usize]) -> Stats {
    let vocab = fit_vocab(inp, rep, train);
    let mut lookup = vec![u32::MAX; id_space(inp, rep)];
    for (d, &id) in vocab.iter().enumerate() {
        lookup[id as usize] = d as u32;
    }
    let dim = vocab.len();
    let proj: Vec<Sparse> = train.iter().map(|r| project(&lookup, r)).collect();
    let mut sizes: HashMap<usize, usize> = HashMap::new();
    for &g in groups {
        *sizes.entry(g).or_default() += 1;
    }
    let w: Vec<f64> = groups.iter().map(|g| 1.0 / sizes[g] as f64).collect();
    let wsum: f64 = w.iter().sum();
    let mut mean = vec![0.0; dim];
    for (p, &wi) in proj.iter().zip(&w) {
        for &(d, x) in p {
            mean[d as usize] += wi * x;
        }
    }
    for m in &mut mean {
        *m /= wsum;
    }
    // Two-pass weighted variance: present coordinates plus absent (x = 0) mass.
    let mut var = vec![0.0; dim];
    let mut wpresent = vec![0.0; dim];
    for (p, &wi) in proj.iter().zip(&w) {
        for &(d, x) in p {
            let e = x - mean[d as usize];
            var[d as usize] += wi * e * e;
            wpresent[d as usize] += wi;
        }
    }
    let sd_total: Vec<f64> = (0..dim)
        .map(|d| {
            let v = (var[d] + (wsum - wpresent[d]).max(0.0) * mean[d] * mean[d]) / wsum;
            v.sqrt().max(SD_FLOOR)
        })
        .collect();
    // Pooled within-author variance with the same document weights.
    let mut by_author: HashMap<usize, Vec<usize>> = HashMap::new();
    for (i, &g) in groups.iter().enumerate() {
        by_author.entry(g).or_default().push(i);
    }
    let mut authors: Vec<usize> = by_author.keys().copied().collect();
    authors.sort_unstable();
    let mut within = vec![0.0; dim];
    let mut scratch_sum = vec![0.0; dim];
    let mut scratch_n = vec![0u32; dim];
    for a in authors {
        let docs = &by_author[&a];
        let na = docs.len() as f64;
        let wa = 1.0 / na; // per-document weight within this author
        let mut touched: Vec<u32> = Vec::new();
        for &i in docs {
            for &(d, x) in &proj[i] {
                if scratch_n[d as usize] == 0 {
                    touched.push(d);
                }
                scratch_sum[d as usize] += x;
                scratch_n[d as usize] += 1;
            }
        }
        for &d in &touched {
            let m = scratch_sum[d as usize] / na;
            let absent = na - scratch_n[d as usize] as f64;
            within[d as usize] += wa * absent * m * m;
            scratch_sum[d as usize] = m; // reuse as the author mean
        }
        for &i in docs {
            for &(d, x) in &proj[i] {
                let e = x - scratch_sum[d as usize];
                within[d as usize] += wa * e * e;
            }
        }
        for &d in &touched {
            scratch_sum[d as usize] = 0.0;
            scratch_n[d as usize] = 0;
        }
    }
    let sd_within: Vec<f64> = within
        .iter()
        .map(|v| (v / wsum).sqrt().max(SD_FLOOR))
        .collect();
    Stats {
        vocab,
        lookup,
        mean,
        sd_total,
        sd_within,
    }
}

/// A fitted configuration: statistics plus the chosen scorer's scale.
#[derive(Clone, Debug)]
pub struct Model {
    pub config: Config,
    pub text: Text,
    pub lookup: Vec<u32>,
    pub dim: usize,
    pub mean: Vec<f64>,
    pub scale: Vec<f64>,
    pub v: Vec<f64>,
    pub vv: f64,
}

impl Model {
    pub fn new(config: Config, text: Text, st: &Stats) -> Self {
        let scale = match config.scorer {
            Scorer::S1 => st.sd_total.clone(),
            Scorer::S2 => st.sd_within.clone(),
        };
        let v: Vec<f64> = st.mean.iter().zip(&scale).map(|(m, s)| m / s).collect();
        let vv = v.iter().map(|x| x * x).sum();
        Model {
            config,
            text,
            lookup: st.lookup.clone(),
            dim: st.vocab.len(),
            mean: st.mean.clone(),
            scale,
            v,
            vv,
        }
    }

    pub fn prepare(&self, r: &Sparse) -> Prepared {
        let mut u: Sparse = Vec::with_capacity(r.len());
        let mut acc = 0.0;
        let mut vsupp = 0.0;
        for &(id, x) in r {
            let d = self.lookup[id as usize];
            if d == u32::MAX {
                continue;
            }
            let ud = x / self.scale[d as usize];
            let vd = self.v[d as usize];
            acc += (ud - vd) * (ud - vd);
            vsupp += vd * vd;
            u.push((d, ud));
        }
        let n2 = acc + (self.vv - vsupp).max(0.0);
        Prepared { u, n: n2.sqrt() }
    }

    /// Digest of the fitted statistics, recorded with the selection.
    pub fn digest(&self) -> String {
        let mut h = Sha256::new();
        h.update(self.config.name().as_bytes());
        h.update(self.text.name().as_bytes());
        h.update((self.dim as u64).to_le_bytes());
        for (m, s) in self.mean.iter().zip(&self.scale) {
            h.update(m.to_bits().to_le_bytes());
            h.update(s.to_bits().to_le_bytes());
        }
        let mut ids: Vec<(u32, u32)> = self
            .lookup
            .iter()
            .enumerate()
            .filter(|(_, d)| **d != u32::MAX)
            .map(|(i, d)| (i as u32, *d))
            .collect();
        ids.sort_unstable();
        for (i, d) in ids {
            h.update(i.to_le_bytes());
            h.update(d.to_le_bytes());
        }
        hex::encode(h.finalize())
    }

    pub fn summary(&self) -> Value {
        json!({"config":self.config.name(),"text":self.text.name(),"dimensions":self.dim,
            "scale_at_floor":self.scale.iter().filter(|s| **s <= SD_FLOOR).count(),
            "stats_sha256":self.digest()})
    }
}

/// A document in a fitted space: u = x / scale (sparse) and n = ||u - v||.
#[derive(Clone, Debug)]
pub struct Prepared {
    pub u: Sparse,
    pub n: f64,
}

/// Unnormalized profile P = sum_i (u_i - v) / n_i (dense), with ||P|| and <v, P>.
pub struct Profile {
    pub p: Vec<f64>,
    pub norm: f64,
    pub vp: f64,
}

pub fn profile(model: &Model, refs: &[&Prepared]) -> Profile {
    let c: f64 = refs.iter().map(|r| 1.0 / r.n).sum();
    let mut p: Vec<f64> = model.v.iter().map(|v| -c * v).collect();
    for r in refs {
        let inv = 1.0 / r.n;
        for &(d, x) in &r.u {
            p[d as usize] += x * inv;
        }
    }
    let norm = p.iter().map(|x| x * x).sum::<f64>().sqrt();
    let vp = p.iter().zip(&model.v).map(|(a, b)| a * b).sum();
    Profile { p, norm, vp }
}

/// Cosine between the normalized standardized query and the profile.
pub fn score(prof: &Profile, q: &Prepared) -> f64 {
    let dot: f64 = q.u.iter().map(|&(d, x)| x * prof.p[d as usize]).sum();
    (dot - prof.vp) / (q.n * prof.norm)
}
