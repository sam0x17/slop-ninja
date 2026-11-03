//! Lookup-risk measurement keys and filters (2026-10-30).
//!
//! A miner whose sandboxed artifact has no network can only "look up" a
//! cohort human document if the artifact itself carries an index of the
//! frame. These are the index keys such a lookup would use, built with
//! the pilot's own tokenisation (`shingle::token_hashes`, folded tokens):
//!
//! * **A, excerpt keys.** One key per possible excerpt: every valid
//!   paragraph run of a page (`candidates::valid_runs`), because the passage
//!   choice is keyed and secret. `A1` is the single chosen excerpt (what a
//!   lookup needs if the passage rule were public).
//! * **B, sentence keys.** One key per distinct sentence of at least
//!   [`MIN_SENT_TOKENS`] folded tokens in the union of a page's valid runs.
//!   Works with any passage rule and anchors partial matches in mixed text.
//! * **W, winnowed 8-gram fingerprints** over the same union (window 8):
//!   any shared stretch of at least 15 tokens leaves a shared fingerprint,
//!   so lightly edited text still matches in part.
//! * **C, MinHash LSH band keys** (16 bands x 8 rows over folded 5-shingles,
//!   the pilot's dedup parameters) for every valid run: near-duplicate
//!   matching at Jaccard about 0.7 and above.
//!
//! Every key is a 64-bit hash. [`Bloom`] is a standard Bloom filter (k
//! remixed positions, multiply-shift reduction), used to validate bits per
//! key and false-positive rates against theory.
use crate::shingle::{self, MH_BANDS, MH_PERMS, MH_SHINGLE, MinHasher, splitmix};
use anyhow::{Context, Result, ensure};
use std::{
    fs::File,
    io::{BufReader, BufWriter, Read, Write},
    path::Path,
};

pub const SPLIT_SEED: &str = "slop_ninja_cc_lookup_v1:2026-10-30";
pub const MIN_SENT_TOKENS: usize = 6;
pub const WIN_N: usize = 8;
pub const WIN_W: usize = 8;
pub const KEY_METHOD: &str = "folded_token_fnv1a;A=all_valid_runs_seqhash;A1=chosen_excerpt_seqhash;B=sentences_ge6_tokens_in_run_union;W=winnow_w8_of_8gram_windows_per_union_block;C=minhash_16x8_5shingle_bands_per_run;v1";

/// Sentence slices: text up to `[.!?]+` plus closing `"')]`, or up to a
/// newline (the cohort-mix `SENT` regex, byte for byte on ASCII delimiters).
pub fn sentences(t: &str) -> Vec<(usize, usize)> {
    let b = t.as_bytes();
    let mut out = Vec::new();
    let (mut s, mut i) = (0usize, 0usize);
    let push = |a: usize, z: usize, out: &mut Vec<(usize, usize)>| {
        if t[a..z].trim().is_empty() {
            return;
        }
        out.push((a, z));
    };
    while i < b.len() {
        match b[i] {
            b'\n' => {
                if i > s {
                    push(s, i, &mut out);
                }
                i += 1;
                s = i;
            }
            b'.' | b'!' | b'?' => {
                let mut j = i + 1;
                while j < b.len() && matches!(b[j], b'.' | b'!' | b'?') {
                    j += 1;
                }
                while j < b.len() && matches!(b[j], b'"' | b'\'' | b')' | b']') {
                    j += 1;
                }
                push(s, j, &mut out);
                s = j;
                i = j;
            }
            _ => i += 1,
        }
    }
    if s < b.len() {
        push(s, b.len(), &mut out);
    }
    out
}

pub fn seq_key(seed: u64, toks: &[u64]) -> u64 {
    let mut h = splitmix(seed ^ (toks.len() as u64).wrapping_mul(0x9E37_79B9));
    for t in toks {
        h = splitmix(h ^ t);
    }
    h
}

const SEED_EXCERPT: u64 = 0xE7C3_A11C_E000_0001;
const SEED_SENT: u64 = 0x5E57_0000_0000_0006;

/// Key of a whole excerpt (folded tokens).
pub fn excerpt_key(t: &str) -> u64 {
    seq_key(SEED_EXCERPT, &shingle::token_hashes(t))
}

/// Sentence key with the sentence's token count (capped at 255) packed in
/// the low byte, so hit rates can be split by length. 56 hash bits remain.
pub fn pack_len(h: u64, n: usize) -> u64 {
    (h & !0xFF) | n.min(255) as u64
}

pub fn key_len(k: u64) -> usize {
    (k & 0xFF) as usize
}

/// Packed keys of every sentence of at least `min_tokens` folded tokens,
/// with each sentence's byte span.
pub fn sentence_keys_spans(t: &str, min_tokens: usize) -> Vec<(u64, usize, usize)> {
    sentences(t)
        .into_iter()
        .filter_map(|(a, z)| {
            let toks = shingle::token_hashes(&t[a..z]);
            (toks.len() >= min_tokens).then(|| (pack_len(seq_key(SEED_SENT, &toks), toks.len()), a, z))
        })
        .collect()
}

pub fn sentence_keys(t: &str) -> Vec<u64> {
    shingle::sorted_unique(sentence_keys_spans(t, MIN_SENT_TOKENS).into_iter().map(|x| x.0).collect())
}

/// Winnowing (window `w`, rightmost minimum) over `n`-token window hashes.
pub fn winnow_tokens(toks: &[u64], n: usize, w: usize) -> Vec<u64> {
    let h = shingle::windows(toks, n);
    if h.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    if h.len() <= w {
        out.push(*h.iter().min().unwrap());
        return out;
    }
    let mut last: Option<usize> = None;
    for i in 0..=h.len() - w {
        let mut m = i;
        for j in i..i + w {
            if h[j] <= h[m] {
                m = j;
            }
        }
        if last != Some(m) {
            out.push(h[m]);
            last = Some(m);
        }
    }
    out
}

pub fn winnow_keys(t: &str) -> Vec<u64> {
    shingle::sorted_unique(winnow_tokens(&shingle::token_hashes(t), WIN_N, WIN_W))
}

pub fn minhash_sig(mh: &MinHasher, t: &str) -> Option<[u64; MH_PERMS]> {
    let sh = shingle::sorted_unique(shingle::windows(&shingle::token_hashes(t), MH_SHINGLE));
    mh.signature(&sh)
}

pub fn lsh_keys(mh: &MinHasher, t: &str) -> Option<[u64; MH_BANDS]> {
    minhash_sig(mh, t).map(|s| MinHasher::bands(&s))
}

/// Count of elements of sorted unique `q` present in sorted unique `s`.
pub fn count_in(q: &[u64], s: &[u64]) -> usize {
    let (mut i, mut j, mut n) = (0, 0, 0);
    while i < q.len() && j < s.len() {
        match q[i].cmp(&s[j]) {
            std::cmp::Ordering::Less => i += 1,
            std::cmp::Ordering::Greater => j += 1,
            std::cmp::Ordering::Equal => {
                n += 1;
                i += 1;
                j += 1;
            }
        }
    }
    n
}

/// A Bloom filter over uniform 64-bit keys.
pub struct Bloom {
    pub bits: Vec<u64>,
    pub m: u64,
    pub k: u32,
    pub n: u64,
}

impl Bloom {
    /// Optimal m and k for `n` keys at false-positive rate `p`.
    pub fn new(n: u64, p: f64) -> Bloom {
        let n = n.max(1);
        let ln2 = std::f64::consts::LN_2;
        let m = ((-(n as f64) * p.ln()) / (ln2 * ln2)).ceil().max(64.0) as u64;
        let k = ((m as f64 / n as f64) * ln2).round().max(1.0) as u32;
        Bloom {
            bits: vec![0; m.div_ceil(64) as usize],
            m,
            k,
            n,
        }
    }
    #[inline]
    fn pos(&self, h: u64, i: u32) -> u64 {
        let x = splitmix(h ^ (i as u64).wrapping_mul(0xD6E8_FEB8_6659_FD93));
        ((x as u128 * self.m as u128) >> 64) as u64
    }
    pub fn insert(&mut self, h: u64) {
        for i in 0..self.k {
            let p = self.pos(h, i);
            self.bits[(p >> 6) as usize] |= 1 << (p & 63);
        }
    }
    pub fn contains(&self, h: u64) -> bool {
        (0..self.k).all(|i| {
            let p = self.pos(h, i);
            self.bits[(p >> 6) as usize] & (1 << (p & 63)) != 0
        })
    }
    pub fn fill(&self) -> f64 {
        self.bits.iter().map(|w| w.count_ones() as u64).sum::<u64>() as f64 / self.m as f64
    }
    /// Theoretical false-positive rate at the measured fill.
    pub fn fpr_at_fill(&self) -> f64 {
        self.fill().powi(self.k as i32)
    }
    pub fn bytes(&self) -> u64 {
        self.m.div_ceil(8)
    }
}

/// The key sets kept per admitted pilot document, in file order.
pub const SETS: [&str; 8] = ["union_a", "union_sent", "union_win", "union_lsh", "ex_a", "ex_sent", "ex_win", "ex_lsh"];

pub struct DocKeys {
    pub idx: u32,
    pub sets: [Vec<u64>; 8],
}

pub fn write_doc(w: &mut impl Write, d: &DocKeys) -> Result<()> {
    w.write_all(&d.idx.to_le_bytes())?;
    for s in &d.sets {
        w.write_all(&(s.len() as u32).to_le_bytes())?;
        for k in s {
            w.write_all(&k.to_le_bytes())?;
        }
    }
    Ok(())
}

pub fn write_docs(path: &Path, docs: &[DocKeys]) -> Result<()> {
    if let Some(p) = path.parent() {
        std::fs::create_dir_all(p)?;
    }
    let tmp = path.with_extension("tmp");
    let mut w = BufWriter::with_capacity(1 << 20, File::create(&tmp)?);
    for d in docs {
        write_doc(&mut w, d)?;
    }
    let f = w.into_inner().map_err(|e| e.into_error())?;
    f.sync_all()?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// Calls `f` for every document of a key file; `want[i]` false skips set i.
pub fn read_docs(path: &Path, want: [bool; 8], mut f: impl FnMut(u32, &[Vec<u64>; 8])) -> Result<u64> {
    let mut r = BufReader::with_capacity(1 << 20, File::open(path).with_context(|| format!("open {}", path.display()))?);
    let mut n = 0u64;
    let mut b4 = [0u8; 4];
    let mut sets: [Vec<u64>; 8] = Default::default();
    loop {
        match r.read_exact(&mut b4) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(e.into()),
        }
        let idx = u32::from_le_bytes(b4);
        for (i, s) in sets.iter_mut().enumerate() {
            r.read_exact(&mut b4)?;
            let len = u32::from_le_bytes(b4) as usize;
            let mut buf = vec![0u8; len * 8];
            r.read_exact(&mut buf)?;
            s.clear();
            if want[i] {
                s.extend(buf.chunks_exact(8).map(|c| u64::from_le_bytes(c.try_into().unwrap())));
            }
        }
        f(idx, &sets);
        n += 1;
    }
    Ok(n)
}

/// Standard normal CDF (erfc via Numerical Recipes' Chebyshev fit, |err| < 1.2e-7).
pub fn phi(x: f64) -> f64 {
    let z = x.abs() / std::f64::consts::SQRT_2;
    let t = 1.0 / (1.0 + 0.5 * z);
    let r = t
        * (-z * z - 1.26551223
            + t * (1.00002368
                + t * (0.37409196
                    + t * (0.09678418 + t * (-0.18628806 + t * (0.27886807 + t * (-1.13520398 + t * (1.48851587 + t * (-0.82215223 + t * 0.17087277)))))))))
            .exp();
    if x >= 0.0 { 1.0 - 0.5 * r } else { 0.5 * r }
}

/// Inverse standard normal CDF (Acklam's rational approximation).
#[allow(clippy::excessive_precision)]
pub fn phi_inv(p: f64) -> f64 {
    ensure_open(p);
    let a = [-3.969683028665376e+01, 2.209460984245205e+02, -2.759285104469687e+02, 1.383577518672690e+02, -3.066479806614716e+01, 2.506628277459239e+00];
    let b = [-5.447609879822406e+01, 1.615858368580409e+02, -1.556989798598866e+02, 6.680131188771972e+01, -1.328068155288572e+01];
    let c = [-7.784894002430293e-03, -3.223964580411365e-01, -2.400758277161838e+00, -2.549732539343734e+00, 4.374664141464968e+00, 2.938163982698783e+00];
    let d = [7.784695709041462e-03, 3.224671290700398e-01, 2.445134137142996e+00, 3.754408661907416e+00];
    let pl = 0.02425;
    if p < pl {
        let q = (-2.0 * p.ln()).sqrt();
        (((((c[0] * q + c[1]) * q + c[2]) * q + c[3]) * q + c[4]) * q + c[5]) / ((((d[0] * q + d[1]) * q + d[2]) * q + d[3]) * q + 1.0)
    } else if p <= 1.0 - pl {
        let q = p - 0.5;
        let r = q * q;
        (((((a[0] * r + a[1]) * r + a[2]) * r + a[3]) * r + a[4]) * r + a[5]) * q / (((((b[0] * r + b[1]) * r + b[2]) * r + b[3]) * r + b[4]) * r + 1.0)
    } else {
        -phi_inv(1.0 - p)
    }
}

fn ensure_open(p: f64) {
    debug_assert!(p > 0.0 && p < 1.0, "probability {p} outside (0, 1)");
}

/// Deterministic uniform in [0, 1) from a counter.
pub fn unif(seed: u64, a: u64, b: u64) -> f64 {
    (splitmix(seed ^ splitmix(a ^ splitmix(b))) >> 11) as f64 / (1u64 << 53) as f64
}

/// Binomial(n, p) draw by inversion of n Bernoulli trials (n is small here).
pub fn binom(n: usize, p: f64, seed: u64, a: u64) -> usize {
    if p <= 0.0 || n == 0 {
        return 0;
    }
    (0..n).filter(|&i| unif(seed, a, i as u64) < p).count()
}

pub fn check_dir(p: &Path) -> Result<()> {
    ensure!(p.is_dir(), "missing directory {}", p.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn sentences_match_regex_semantics() {
        let t = "One two three. \"Four five?\" Six\nseven eight! Tail";
        let s: Vec<&str> = sentences(t).iter().map(|&(a, z)| &t[a..z]).collect();
        assert_eq!(s, vec!["One two three.", " \"Four five?\"", " Six", "seven eight!", " Tail"]);
    }
    #[test]
    fn keys_are_folding_invariant() {
        assert_eq!(excerpt_key("Hello,  World!"), excerpt_key("hello world"));
        let a = sentence_keys("The cat sat on the warm mat today. Short one.");
        assert_eq!(a.len(), 1);
        assert_eq!(key_len(a[0]), 8);
        assert_eq!(a, sentence_keys("the CAT sat on the warm mat, today!"));
    }
    #[test]
    fn winnowing_guarantees_shared_fingerprint() {
        let base: Vec<String> = (0..120).map(|i| format!("w{i}")).collect();
        let a = base.join(" ");
        let mut b2 = base.clone();
        b2[60] = "changed".into();
        let b = b2.join(" ");
        let ka = winnow_keys(&a);
        let kb = winnow_keys(&b);
        let shared = count_in(&kb, &ka);
        assert!(shared > 0 && shared < kb.len(), "{shared} of {}", kb.len());
        assert!(ka.len() * 10 < 120 * 4, "density {}", ka.len());
    }
    #[test]
    fn bloom_fpr_near_target() {
        let n = 200_000u64;
        let mut b = Bloom::new(n, 0.01);
        for i in 0..n {
            b.insert(splitmix(i));
        }
        assert!((0..n).all(|i| b.contains(splitmix(i))));
        let fp = (0..400_000u64).filter(|i| b.contains(splitmix(i + 10_000_000))).count() as f64 / 400_000.0;
        assert!((0.007..0.013).contains(&fp), "fpr {fp}");
        let bpk = b.m as f64 / n as f64;
        assert!((9.5..9.7).contains(&bpk), "bits per key {bpk}");
    }
    #[test]
    fn normal_helpers() {
        assert!((phi(1.96) - 0.975).abs() < 1e-4);
        assert!((phi_inv(0.999) - 3.0902).abs() < 1e-3);
        assert!((phi(phi_inv(0.2)) - 0.2).abs() < 1e-6);
    }
    #[test]
    fn key_file_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("k.bin");
        let d = DocKeys {
            idx: 7,
            sets: [vec![1], vec![2, 3], vec![], vec![4], vec![5], vec![6], vec![7], vec![8, 9]],
        };
        write_docs(&p, &[d]).unwrap();
        let mut seen = Vec::new();
        read_docs(&p, [true, true, false, true, true, true, true, true], |i, s| seen.push((i, s[1].clone(), s[2].len(), s[7].clone()))).unwrap();
        assert_eq!(seen, vec![(7, vec![2, 3], 0, vec![8, 9])]);
    }
}
