//! Allocation-free folded token hashes, rolling n-word window hashes and
//! MinHash signatures.
//!
//! Tokens follow `grammar_a_pools::text::folded_tokens` exactly (maximal
//! runs of alphanumeric characters, lowercased); each token is hashed with
//! FNV-1a 64 over its lowercased UTF-8 bytes, and an n-word window is a
//! polynomial rolling hash of its token hashes, finalised with splitmix64.
//! These hashes are internal index keys: both sides of every comparison use
//! them, and a collision can only exclude a candidate.

pub const W12: usize = 12;
pub const W50: usize = 50;
pub const MH_SHINGLE: usize = 5;
pub const MH_BANDS: usize = 16;
pub const MH_ROWS: usize = 8;
pub const MH_PERMS: usize = MH_BANDS * MH_ROWS;
pub const MH_VERIFY_JACCARD: f64 = 0.5;

pub fn splitmix(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9E3779B97F4A7C15);
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D049BB133111EB);
    x ^ (x >> 31)
}

pub fn token_hashes(s: &str) -> Vec<u64> {
    let mut out = Vec::new();
    let mut h: u64 = 0xcbf29ce484222325;
    let mut any = false;
    let mut buf = [0u8; 4];
    for c in s.chars() {
        if c.is_alphanumeric() {
            for l in c.to_lowercase() {
                for b in l.encode_utf8(&mut buf).as_bytes() {
                    h ^= *b as u64;
                    h = h.wrapping_mul(0x100000001b3);
                }
            }
            any = true;
        } else if any {
            out.push(h);
            h = 0xcbf29ce484222325;
            any = false;
        }
    }
    if any {
        out.push(h);
    }
    out
}

const B: u64 = 0x0000_0100_0000_01b3 | 0x5bd1_e995_0000_0000;

/// Hash of every `n`-token window (in order; empty when fewer tokens).
pub fn windows(toks: &[u64], n: usize) -> Vec<u64> {
    if toks.len() < n || n == 0 {
        return Vec::new();
    }
    let mut bn = 1u64; // B^(n-1)
    for _ in 1..n {
        bn = bn.wrapping_mul(B);
    }
    let mut h = 0u64;
    for t in &toks[..n] {
        h = h.wrapping_mul(B).wrapping_add(*t);
    }
    let salt = splitmix(n as u64);
    let mut out = Vec::with_capacity(toks.len() - n + 1);
    out.push(splitmix(h ^ salt));
    for i in n..toks.len() {
        h = h.wrapping_sub(toks[i - n].wrapping_mul(bn)).wrapping_mul(B).wrapping_add(toks[i]);
        out.push(splitmix(h ^ salt));
    }
    out
}

pub fn sorted_unique(mut v: Vec<u64>) -> Vec<u64> {
    v.sort_unstable();
    v.dedup();
    v
}

pub struct MinHasher {
    a: [u64; MH_PERMS],
    b: [u64; MH_PERMS],
}

impl Default for MinHasher {
    fn default() -> Self {
        Self::new()
    }
}

impl MinHasher {
    pub fn new() -> MinHasher {
        let mut a = [0u64; MH_PERMS];
        let mut b = [0u64; MH_PERMS];
        for k in 0..MH_PERMS {
            a[k] = splitmix(0xA11CE ^ (k as u64) << 1) | 1;
            b[k] = splitmix(0xB0B ^ (k as u64) << 7);
        }
        MinHasher { a, b }
    }
    /// Signature over a set of shingle hashes; None for an empty set.
    pub fn signature(&self, shingles: &[u64]) -> Option<[u64; MH_PERMS]> {
        if shingles.is_empty() {
            return None;
        }
        let mut m = [u64::MAX; MH_PERMS];
        for &x in shingles {
            for k in 0..MH_PERMS {
                let v = self.a[k].wrapping_mul(x).wrapping_add(self.b[k]);
                if v < m[k] {
                    m[k] = v;
                }
            }
        }
        Some(m)
    }
    pub fn bands(sig: &[u64; MH_PERMS]) -> [u64; MH_BANDS] {
        let mut out = [0u64; MH_BANDS];
        for (j, o) in out.iter_mut().enumerate() {
            let mut h = splitmix(0xBA4D ^ j as u64);
            for r in 0..MH_ROWS {
                h = splitmix(h ^ sig[j * MH_ROWS + r]);
            }
            *o = h;
        }
        out
    }
}

/// Jaccard similarity of two sorted unique sets.
pub fn jaccard(a: &[u64], b: &[u64]) -> f64 {
    if a.is_empty() && b.is_empty() {
        return 1.0;
    }
    let (mut i, mut j, mut inter) = (0, 0, 0usize);
    while i < a.len() && j < b.len() {
        match a[i].cmp(&b[j]) {
            std::cmp::Ordering::Less => i += 1,
            std::cmp::Ordering::Greater => j += 1,
            std::cmp::Ordering::Equal => {
                inter += 1;
                i += 1;
                j += 1;
            }
        }
    }
    inter as f64 / (a.len() + b.len() - inter) as f64
}

/// Whether two sorted unique sets share an element.
pub fn intersects(a: &[u64], b: &[u64]) -> bool {
    let (mut i, mut j) = (0, 0);
    while i < a.len() && j < b.len() {
        match a[i].cmp(&b[j]) {
            std::cmp::Ordering::Less => i += 1,
            std::cmp::Ordering::Greater => j += 1,
            std::cmp::Ordering::Equal => return true,
        }
    }
    false
}

/// A sorted array of packed keys: the top `64 - ID_BITS` bits of a hash
/// and an id in the low bits. Lookups start at the interpolated position
/// (hashes are uniform) and return every id whose prefix matches; callers
/// confirm the full hash.
pub const ID_BITS: u32 = 21;
pub const ID_MASK: u64 = (1 << ID_BITS) - 1;

pub struct Packed {
    pub keys: Vec<u64>,
}

impl Packed {
    pub fn build(mut keys: Vec<u64>) -> Packed {
        keys.sort_unstable();
        keys.dedup();
        Packed { keys }
    }
    pub fn pack(h: u64, id: u32) -> u64 {
        debug_assert!((id as u64) <= ID_MASK);
        (h & !ID_MASK) | id as u64
    }
    /// First index whose key is >= x: an interpolated guess (keys are
    /// uniform hashes), widened until it brackets x, then a binary search.
    fn lower_bound(&self, x: u64) -> usize {
        let k = &self.keys;
        let n = k.len();
        if n == 0 {
            return 0;
        }
        let guess = (((x as u128) * (n as u128)) >> 64) as usize;
        let mut d = 64usize;
        loop {
            let lo = guess.saturating_sub(d);
            let hi = (guess + d).min(n);
            if (lo == 0 || k[lo - 1] < x) && (hi == n || k[hi] >= x) {
                return lo + k[lo..hi].partition_point(|&v| v < x);
            }
            d = d.saturating_mul(4);
        }
    }
    /// Ids whose packed prefix equals the prefix of `h`.
    pub fn ids(&self, h: u64, out: &mut Vec<u32>) {
        let p = h & !ID_MASK;
        let mut i = self.lower_bound(p);
        while i < self.keys.len() && (self.keys[i] & !ID_MASK) == p {
            out.push((self.keys[i] & ID_MASK) as u32);
            i += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn tokens_match_folded_tokens() {
        let s = "Hello, WORLD! Ça va? x2 don't-stop";
        let a = token_hashes(s);
        let b: Vec<u64> = grammar_a_pools::text::folded_tokens(s).iter().map(|t| token_hashes(t)[0]).collect();
        assert_eq!(a, b);
        assert_eq!(a.len(), 8);
    }
    #[test]
    fn rolling_windows_equal_direct() {
        let t = token_hashes("a b c d e f g h");
        let w = windows(&t, 3);
        assert_eq!(w.len(), 6);
        assert_eq!(windows(&t[2..5], 3)[0], w[2]);
        assert!(windows(&t, 9).is_empty());
    }
    #[test]
    fn packed_lookup_and_jaccard() {
        let hs: Vec<u64> = (0..5000u64).map(splitmix).collect();
        let p = Packed::build(hs.iter().enumerate().map(|(i, h)| Packed::pack(*h, i as u32)).collect());
        for (i, h) in hs.iter().enumerate() {
            let mut v = Vec::new();
            p.ids(*h, &mut v);
            assert!(v.contains(&(i as u32)), "{i}");
        }
        let mut v = Vec::new();
        p.ids(splitmix(999_999), &mut v);
        assert!(v.is_empty());
        assert_eq!(jaccard(&[1, 2, 3], &[2, 3, 4]), 0.5);
        assert!(intersects(&[1, 5], &[5, 9]) && !intersects(&[1], &[2]));
        let m = MinHasher::new();
        let a = sorted_unique(windows(&token_hashes("one two three four five six"), MH_SHINGLE));
        let s1 = m.signature(&a).unwrap();
        assert_eq!(MinHasher::bands(&s1), MinHasher::bands(&m.signature(&a).unwrap()));
        assert!(m.signature(&[]).is_none());
    }
}
