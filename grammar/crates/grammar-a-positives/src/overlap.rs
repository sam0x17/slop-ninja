//! Generated-text overlap against all prior material: the hash-bound
//! protected index (`slop_ninja_a_pool_protected_index_v1`, read only), extra
//! human populations not in it, and every generated text accepted so far.
use anyhow::{Context, Result, ensure};
use grammar_a_pools::text as ptext;
use serde_json::Value;
use std::{
    collections::HashSet,
    fs,
    path::Path,
    sync::Mutex,
};

const KINDS: [&str; 5] = ["exact", "normalized", "folded", "shingles_ws", "shingles_folded"];

pub struct PriorIndex {
    keys: [Vec<u64>; 5],
    pub sha256: String,
    pub path: String,
}

impl PriorIndex {
    pub fn load(dir: &Path) -> Result<Self> {
        let raw = fs::read(dir.join("index.json")).context("index.json")?;
        let meta: Value = serde_json::from_slice(&raw)?;
        ensure!(meta["schema"] == "slop_ninja_a_pool_protected_index_v1", "index schema");
        let mut keys: [Vec<u64>; 5] = Default::default();
        for (i, k) in KINDS.iter().enumerate() {
            let b = fs::read(dir.join(format!("{k}.u64")))?;
            ensure!(ptext::sha_hex(&b) == meta["key_files"][*k]["sha256"], "index key file {k} changed");
            keys[i] = b.chunks_exact(8).map(|c| u64::from_le_bytes(c.try_into().unwrap())).collect();
        }
        Ok(PriorIndex { keys, sha256: ptext::sha_hex(&raw), path: dir.display().to_string() })
    }
    pub fn empty() -> Self {
        PriorIndex { keys: Default::default(), sha256: "empty".into(), path: String::new() }
    }
    fn has(&self, kind: usize, k: u64) -> bool {
        self.keys[kind].binary_search(&k).is_ok()
    }
}

/// Keys of one text: whole-text exact/normalized/folded plus 50-word windows.
pub struct Keys {
    pub whole: [u64; 3],
    pub ws: Vec<u64>,
    pub folded: Vec<u64>,
}
pub fn keys(s: &str) -> Keys {
    Keys { whole: ptext::text_keys(s), ws: ptext::shingles_ws(s), folded: ptext::shingles_folded(s) }
}

pub struct Overlap {
    pub prior: PriorIndex,
    /// Human texts outside the index (for example CAL), as keys.
    extra: HashSet<(u8, u64)>,
    /// Accepted generated texts (all splits, all generators).
    generated: Mutex<HashSet<(u8, u64)>>,
}

fn tagged(k: &Keys) -> Vec<(u8, u64)> {
    let mut v: Vec<(u8, u64)> = k.whole.iter().enumerate().map(|(i, &x)| (i as u8, x)).collect();
    v.extend(k.ws.iter().map(|&x| (3u8, x)));
    v.extend(k.folded.iter().map(|&x| (4u8, x)));
    v
}

impl Overlap {
    pub fn new(prior: PriorIndex) -> Self {
        Overlap { prior, extra: HashSet::new(), generated: Mutex::new(HashSet::new()) }
    }
    pub fn add_extra(&mut self, s: &str) {
        for t in tagged(&keys(s)) {
            self.extra.insert(t);
        }
    }
    pub fn add_generated(&self, s: &str) {
        let mut g = self.generated.lock().unwrap();
        for t in tagged(&keys(s)) {
            g.insert(t);
        }
    }
    /// Conflict labels for generated text `s`. Windows that also occur in
    /// `own_parent` are ignored when `exclude_parent` (rewrites may keep
    /// names and phrases of the passage they rewrite).
    pub fn conflicts(&self, s: &str, own_parent: Option<&str>) -> Vec<String> {
        let k = keys(s);
        let parent: HashSet<(u8, u64)> = own_parent.map(|p| tagged(&keys(p)).into_iter().collect()).unwrap_or_default();
        let mut out = Vec::new();
        let names = ["exact_text", "normalized_text", "folded_text", "50_word_sequence", "folded_50_word_sequence"];
        let generated = self.generated.lock().unwrap();
        for (kind, x) in tagged(&k) {
            if parent.contains(&(kind, x)) {
                continue;
            }
            let i = kind as usize;
            if self.prior.has(i, x) || self.extra.contains(&(kind, x)) {
                out.push(format!("prior_{}", names[i]));
            }
            if generated.contains(&(kind, x)) {
                out.push(format!("generated_{}", names[i]));
            }
        }
        out.sort();
        out.dedup();
        out
    }
}

/// Longest run of folded tokens of `a` that occurs contiguously in `b`
/// (checked by windows of `n`): true when any `n`-window is shared.
pub fn shares_window(a: &str, b: &str, n: usize) -> bool {
    let bw: HashSet<u64> = crate::text::folded_windows(b, n).into_iter().collect();
    crate::text::folded_windows(a, n).into_iter().any(|x| bw.contains(&x))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn prose(seed: &str, n: usize) -> String {
        (0..n).map(|i| format!("{seed}{i}")).collect::<Vec<_>>().join(" ")
    }
    #[test]
    fn parent_windows_are_excluded_only_when_asked() {
        let mut o = Overlap::new(PriorIndex::empty());
        let parent = prose("p", 80);
        o.add_extra(&parent);
        let copy = format!("{} new words here", prose("p", 60));
        assert!(o.conflicts(&copy, None).iter().any(|c| c == "prior_50_word_sequence"));
        assert!(o.conflicts(&copy, Some(&parent)).is_empty());
        o.add_generated(&copy);
        assert!(o.conflicts(&copy, Some(&parent)).iter().any(|c| c.starts_with("generated_exact")));
    }
    #[test]
    fn short_window_sharing() {
        assert!(shares_window("a b c d e f g h i j k l m", "x a b c d e f g h i j k l m y", 12));
        assert!(!shares_window("a b c d e f g h i j k", "a b c d e f g h i j k", 12));
    }
}
