//! Output style breadth: topic-light style profiles of generated texts
//! (function-word rates, punctuation and layout rates, sentence-length
//! moments, casing), grouped by style kind / generator / cohort. Reports
//! dispersion (1 - mean pairwise cosine over a keyed sample of pairs),
//! leader-cluster counts at fixed cosine thresholds and nearest-neighbour
//! similarity. A lexical diagnostic only: it measures variety, not whether a
//! voice was followed.
use crate::text;
use serde_json::{Value, json};
use std::collections::BTreeMap;

pub const FUNCTION_WORDS: &[&str] = &[
    "the", "a", "an", "and", "or", "but", "of", "to", "in", "on", "at", "by", "for", "with", "from", "as", "that", "which", "who", "this",
    "these", "those", "it", "its", "i", "me", "my", "we", "our", "you", "your", "he", "him", "his", "she", "her", "they", "them", "their",
    "is", "are", "was", "were", "be", "been", "being", "have", "has", "had", "do", "does", "did", "will", "would", "shall", "should", "can",
    "could", "may", "might", "must", "not", "no", "so", "if", "then", "than", "when", "where", "while", "because", "though", "although",
    "there", "here", "what", "how", "why", "all", "some", "any", "very", "just", "also", "only", "even", "yet", "still", "now", "oh", "well",
    "like", "really", "ye", "thee", "thou", "ain't", "gonna", "y'all", "upon", "whilst", "shall",
];
pub const PUNCT: &[char] = &[',', '.', ';', ':', '!', '?', '-', '\u{2014}', '(', '"', '\'', '\u{2019}', '*', '#', '\n'];

pub fn profile(t: &str) -> Vec<f32> {
    let words: Vec<String> = t
        .split_whitespace()
        .map(|w| w.trim_matches(|c: char| !c.is_alphanumeric() && c != '\'').to_lowercase())
        .filter(|w| !w.is_empty())
        .collect();
    let n = words.len().max(1) as f32;
    let mut v = vec![0f32; FUNCTION_WORDS.len() + PUNCT.len() + 6];
    for w in &words {
        if let Some(i) = FUNCTION_WORDS.iter().position(|f| f == w) {
            v[i] += 1.0 / n;
        }
    }
    let chars = t.chars().count().max(1) as f32;
    for c in t.chars() {
        if let Some(i) = PUNCT.iter().position(|p| *p == c) {
            v[FUNCTION_WORDS.len() + i] += 20.0 / chars;
        }
    }
    let k = FUNCTION_WORDS.len() + PUNCT.len();
    let sents = text::sentences(t);
    let lens: Vec<f32> = sents.iter().map(|s| text::words(&t[s.start..s.end]) as f32).collect();
    let m = lens.iter().sum::<f32>() / lens.len().max(1) as f32;
    let sd = (lens.iter().map(|x| (x - m).powi(2)).sum::<f32>() / lens.len().max(1) as f32).sqrt();
    v[k] = (m / 40.0).min(2.0);
    v[k + 1] = (sd / 40.0).min(2.0);
    let letters: Vec<char> = t.chars().filter(|c| c.is_alphabetic()).collect();
    let upper = letters.iter().filter(|c| c.is_uppercase()).count() as f32 / letters.len().max(1) as f32;
    v[k + 2] = upper * 4.0;
    v[k + 3] = (t.chars().filter(|c| (*c as u32) > 0x2000 && !c.is_alphanumeric() && !PUNCT.contains(c)).count() as f32 / chars) * 50.0;
    v[k + 4] = words.iter().map(|w| w.chars().count() as f32).sum::<f32>() / n / 10.0;
    v[k + 5] = (words.iter().collect::<std::collections::HashSet<_>>().len() as f32 / n).min(1.0);
    let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        for x in v.iter_mut() {
            *x /= norm;
        }
    }
    v
}

fn cos(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

/// Breadth of one group of profiles (deterministically subsampled to `cap`).
pub fn group_breadth(profiles: &[Vec<f32>], cap: usize, seed: &str) -> Value {
    let mut idx: Vec<usize> = (0..profiles.len()).collect();
    let mut rng = crate::library::Rng::new(seed);
    for i in (1..idx.len()).rev() {
        let j = rng.below(i + 1);
        idx.swap(i, j);
    }
    idx.truncate(cap);
    let ps: Vec<&Vec<f32>> = idx.iter().map(|&i| &profiles[i]).collect();
    let n = ps.len();
    if n < 2 {
        return json!({"n":n});
    }
    let mut sum = 0f64;
    let mut cnt = 0usize;
    let mut nn = vec![-1f32; n];
    for i in 0..n {
        for k in (i + 1)..n {
            let c = cos(ps[i], ps[k]);
            sum += c as f64;
            cnt += 1;
            nn[i] = nn[i].max(c);
            nn[k] = nn[k].max(c);
        }
    }
    let mut clusters = serde_json::Map::new();
    for tau in [0.9f32, 0.95] {
        let mut leaders: Vec<usize> = Vec::new();
        for i in 0..n {
            if !leaders.iter().any(|&l| cos(ps[i], ps[l]) >= tau) {
                leaders.push(i);
            }
        }
        clusters.insert(format!("leader_clusters_tau_{tau}"), json!(leaders.len()));
    }
    let mut v = json!({"n":n,"population":profiles.len(),"dispersion_1_minus_mean_pairwise_cosine":1.0 - sum / cnt as f64,
        "nearest_neighbour_cosine":crate::library::quantiles(nn)});
    for (k, x) in clusters {
        v[k] = x;
    }
    v
}

/// Group records (label -> profiles) and report breadth per label.
pub fn report(groups: &BTreeMap<String, Vec<Vec<f32>>>, cap: usize) -> Value {
    let mut m = serde_json::Map::new();
    for (k, v) in groups {
        m.insert(k.clone(), group_breadth(v, cap, k));
    }
    Value::Object(m)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn profiles_separate_registers() {
        let a = profile("Arr, me hearties! Ye be sailin' the seas, an' ye'd best mind the gulls!!");
        let b = profile("The committee shall review the application, and the decision shall be published in the register.");
        let c = profile("The board shall examine the request, and the outcome shall be recorded in the official register.");
        assert!(cos(&b, &c) > cos(&a, &b));
        let g = group_breadth(&[a, b, c], 10, "t");
        assert_eq!(g["n"], 3);
    }
}
