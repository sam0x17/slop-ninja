//! Sealed text rules for positive construction: whitespace words, the
//! sentence splitter (re-exported from `grammar-a-form`), TF-IDF key terms
//! and response screens.
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap};

pub const TERMS_ID: &str = "a_comp_tfidf_terms_v1:lower_alpha_runs_ge4_stopwords_v1_idf_ln((1+N)/(1+df))+1_tf_count_top8_ties_first_occurrence";
pub const WORD_POLICY: &str = "unicode_split_whitespace_v1";

pub fn sha_hex(b: &[u8]) -> String {
    hex::encode(Sha256::digest(b))
}

pub fn words(s: &str) -> usize {
    s.split_whitespace().count()
}

/// The sealed sentence splitter `a_comp_sentences_v1` (`sentences`, with
/// `Sentence` and `SPLITTER_ID`) is `grammar_a_form::text`'s, the one source
/// shared with the form path, the cohort tools and the coordinator's port.
pub use grammar_a_form::text::{SPLITTER_ID, Sentence, sentences};

/// Fixed stopword list for key-term extraction (lowercase; only words of at
/// least four letters matter because shorter types are already excluded).
pub const STOPWORDS_V1: &[&str] = &[
    "about", "above", "across", "after", "afterwards", "again", "against", "almost", "alone", "along", "already",
    "also", "although", "always", "among", "amongst", "another", "anyhow", "anyone", "anything", "anyway",
    "anywhere", "around", "away", "back", "became", "because", "become", "becomes", "becoming", "been", "before",
    "beforehand", "behind", "being", "below", "beside", "besides", "between", "beyond", "both", "came", "cannot",
    "could", "does", "doing", "done", "down", "during", "each", "either", "else", "elsewhere", "enough", "even",
    "ever", "every", "everyone", "everything", "everywhere", "except", "first", "from", "further", "gave", "give",
    "given", "gives", "going", "gone", "have", "having", "hence", "here", "hereafter", "hereby", "herein",
    "hers", "herself", "himself", "however", "indeed", "instead", "into", "itself", "just", "keep", "last",
    "latter", "least", "less", "like", "made", "make", "makes", "many", "might", "mine", "more", "moreover",
    "most", "mostly", "much", "must", "myself", "namely", "neither", "never", "nevertheless", "next", "nobody",
    "none", "noone", "nothing", "now", "nowhere", "often", "once", "only", "onto", "other", "others",
    "otherwise", "ours", "ourselves", "over", "own", "part", "perhaps", "please", "quite", "rather", "really",
    "said", "same", "says", "seem", "seemed", "seeming", "seems", "several", "shall", "should", "show", "side",
    "since", "some", "somehow", "someone", "something", "sometime", "sometimes", "somewhere", "still", "such",
    "take", "taken", "than", "that", "their", "theirs", "them", "themselves", "then", "thence", "there",
    "thereafter", "thereby", "therefore", "therein", "thereupon", "these", "they", "thing", "things", "this",
    "those", "though", "three", "through", "throughout", "thru", "thus", "together", "took", "toward",
    "towards", "under", "unless", "until", "upon", "used", "using", "very", "want", "well", "were", "what",
    "whatever", "when", "whence", "whenever", "where", "whereafter", "whereas", "whereby", "wherein",
    "whereupon", "wherever", "whether", "which", "while", "whither", "whoever", "whole", "whom", "whose",
    "will", "with", "within", "without", "would", "your", "yours", "yourself", "yourselves", "upon", "unto",
    "thee", "thou", "thy", "thine", "hath", "doth", "shalt", "wilt",
];

/// Lowercased alphabetic runs of at least four letters, stopwords removed,
/// in order of occurrence.
pub fn term_tokens(s: &str, stop: &std::collections::HashSet<&'static str>) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let flush = |cur: &mut String, out: &mut Vec<String>| {
        if cur.chars().count() >= 4 && !stop.contains(cur.as_str()) {
            out.push(std::mem::take(cur));
        } else {
            cur.clear();
        }
    };
    for c in s.chars() {
        if c.is_alphabetic() {
            cur.extend(c.to_lowercase());
        } else if !cur.is_empty() {
            flush(&mut cur, &mut out);
        }
    }
    if !cur.is_empty() {
        flush(&mut cur, &mut out);
    }
    out
}

pub fn stopwords() -> std::collections::HashSet<&'static str> {
    STOPWORDS_V1.iter().copied().collect()
}

/// Document frequencies of one domain's Train pool.
#[derive(Default, Clone)]
pub struct Idf {
    pub n_docs: usize,
    pub df: HashMap<String, usize>,
}

impl Idf {
    pub fn add(&mut self, text: &str, stop: &std::collections::HashSet<&'static str>) {
        self.n_docs += 1;
        let mut seen = std::collections::HashSet::new();
        for t in term_tokens(text, stop) {
            if seen.insert(t.clone()) {
                *self.df.entry(t).or_insert(0) += 1;
            }
        }
    }
    pub fn idf(&self, t: &str) -> f64 {
        let df = self.df.get(t).copied().unwrap_or(0) as f64;
        ((1.0 + self.n_docs as f64) / (1.0 + df)).ln() + 1.0
    }
    /// Canonical digest of the table (sorted terms).
    pub fn digest(&self) -> String {
        let sorted: BTreeMap<&String, &usize> = self.df.iter().collect();
        let mut h = Sha256::new();
        h.update(format!("n={}\n", self.n_docs).as_bytes());
        for (k, v) in sorted {
            h.update(format!("{k}\t{v}\n").as_bytes());
        }
        hex::encode(h.finalize())
    }
}

/// Eight highest TF-IDF word types; ties by first occurrence.
pub fn key_terms(text: &str, idf: &Idf, stop: &std::collections::HashSet<&'static str>, k: usize) -> Vec<String> {
    let toks = term_tokens(text, stop);
    let mut order: Vec<String> = Vec::new();
    let mut tf: HashMap<String, usize> = HashMap::new();
    for t in toks {
        let e = tf.entry(t.clone()).or_insert(0);
        if *e == 0 {
            order.push(t);
        }
        *e += 1;
    }
    let mut scored: Vec<(usize, f64, String)> =
        order.into_iter().enumerate().map(|(i, t)| (i, tf[&t] as f64 * idf.idf(&t), t)).collect();
    scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap().then(a.0.cmp(&b.0)));
    scored.into_iter().take(k).map(|x| x.2).collect()
}

/// Chat-template control tokens that must never appear in accepted prose.
pub const SPECIAL_MARKERS: &[&str] = &[
    "<|", "|>", "<think>", "</think>", "<end_of_turn>", "<start_of_turn>", "</s>", "<s>", "[INST]", "[/INST]",
    "<eos>", "<bos>", "<pad>", "<unk>", "[gMASK]", "<sop>", "<eop>", "<mask>", "<sep>", "<cls>",
];

pub fn special_tokens(s: &str) -> Vec<&'static str> {
    SPECIAL_MARKERS.iter().copied().filter(|m| s.contains(m)).collect()
}

/// Remove only a verified empty leading reasoning block (some templates emit
/// one with reasoning disabled), then trim. Nonempty reasoning is not prose.
pub fn strip_empty_think(content: &str) -> Result<String, &'static str> {
    let t = content.trim();
    if let Some(rest) = t.strip_prefix("<think>") {
        let (r, body) = rest.split_once("</think>").ok_or("unclosed_reasoning_block")?;
        if !r.trim().is_empty() {
            return Err("nonempty_reasoning_in_content");
        }
        Ok(body.trim().to_owned())
    } else {
        Ok(t.to_owned())
    }
}

/// Cut base-model output at the last punctuated sentence end whose words
/// stay within `budget`. Returns the cut text (trimmed) or None.
pub fn cut_to_budget(raw: &str, budget: usize) -> Option<String> {
    let g = raw.trim_start();
    let mut best: Option<usize> = None;
    for s in sentences(g) {
        if !s.punctuated {
            continue;
        }
        if words(&g[..s.end]) <= budget {
            best = Some(s.end);
        } else {
            break;
        }
    }
    best.map(|e| g[..e].trim().to_owned()).filter(|t| !t.is_empty())
}

/// Hashes of every n-token folded window (lowercase alphanumeric tokens).
pub fn folded_windows(s: &str, n: usize) -> Vec<u64> {
    let w = grammar_a_pools::text::folded_tokens(s);
    w.windows(n).map(|x| grammar_a_pools::text::h64(format!("w{n}\0{}", x.join(" ")).as_bytes())).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn splitter_boundaries_and_whitespace() {
        let t = "One two. \"Three four?\" Five six\n\nSeven eight! U.S. nine";
        let s = sentences(t);
        let got: Vec<&str> = s.iter().map(|x| &t[x.start..x.end]).collect();
        assert_eq!(got, vec!["One two.", "\"Three four?\"", "Five six", "Seven eight!", "U.S.", "nine"]);
        assert!(s[0].punctuated && !s[2].punctuated && !s[5].punctuated);
        for x in &s {
            assert!(x.start == 0 || t[..x.start].ends_with(char::is_whitespace));
            assert!(x.end == t.len() || t[x.end..].starts_with(char::is_whitespace));
        }
    }
    #[test]
    fn splitter_crlf_blank_lines_and_inner_dots() {
        let t = "A line\r\nwrapped here. Next 3.5 units\r\n\r\nNew para";
        let s = sentences(t);
        let got: Vec<&str> = s.iter().map(|x| &t[x.start..x.end]).collect();
        assert_eq!(got, vec!["A line\r\nwrapped here.", "Next 3.5 units", "New para"]);
    }
    #[test]
    fn terms_tfidf_ties_by_first_occurrence() {
        let stop = stopwords();
        let mut idf = Idf::default();
        idf.add("river bank water", &stop);
        idf.add("river mill", &stop);
        let t = key_terms("The mill and the river; water water, bank. Zebra", &idf, &stop, 3);
        // water tf2 idf ln(3/2)+1; zebra tf1 idf ln3+1 (2.0986) vs water 2*1.405=2.81
        assert_eq!(t[0], "water");
        assert_eq!(t[1], "zebra");
        assert_eq!(t[2], "mill");
    }
    #[test]
    fn stopwords_and_short_words_removed() {
        let stop = stopwords();
        assert_eq!(term_tokens("They were THERE with cats, dogs and owl's nests", &stop), vec!["cats", "dogs", "nests"]);
    }
    #[test]
    fn specials_and_think() {
        assert_eq!(special_tokens("ok <|im_end|>"), vec!["<|", "|>"]);
        assert!(special_tokens("plain prose.").is_empty());
        assert_eq!(strip_empty_think("<think>\n\n</think>\n\nBody.").unwrap(), "Body.");
        assert!(strip_empty_think("<think>x</think>Body").is_err());
    }
    #[test]
    fn cut_at_last_sentence_within_budget() {
        assert_eq!(cut_to_budget("  One two three. Four five. Six seven eight nine", 5).unwrap(), "One two three. Four five.");
        assert_eq!(cut_to_budget("One two three. Four five.", 2), None);
        assert_eq!(cut_to_budget("no end here", 10), None);
    }
}
