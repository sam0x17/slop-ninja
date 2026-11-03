//! Filter funnel: documents and words surviving each stage, by domain.
//!
//! `records` counts every WET conversion record; each later stage counts the
//! records that pass it and every earlier stage. Page stages count page
//! words (whitespace tokens of the WET text); from `excerpt` on, excerpt
//! words are counted too.
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const STAGES: &[&str] = &[
    "records",
    "date_gate",
    "url_gate",
    "length_gate",
    "gopher_quality",
    "gopher_repetition",
    "machine_text",
    "rights_text",
    "excerpt",
    "english",
    "corpus_v1_dedup",
    "host_closure",
    "pool_dedup",
];

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Cell {
    pub docs: u64,
    pub page_words: u64,
    pub excerpt_words: u64,
}

impl Cell {
    pub fn add(&mut self, o: &Cell) {
        self.docs += o.docs;
        self.page_words += o.page_words;
        self.excerpt_words += o.excerpt_words;
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Funnel {
    /// stage -> domain -> survivors.
    pub cells: BTreeMap<String, BTreeMap<String, Cell>>,
    /// rejecting stage -> reason -> domain -> count.
    pub reasons: BTreeMap<String, BTreeMap<String, BTreeMap<String, u64>>>,
}

impl Funnel {
    /// Count a document as surviving `stages` (in order).
    pub fn pass(&mut self, stage: &str, domain: &str, page_words: usize, excerpt_words: usize) {
        let c = self.cells.entry(stage.to_owned()).or_default().entry(domain.to_owned()).or_default();
        c.docs += 1;
        c.page_words += page_words as u64;
        c.excerpt_words += excerpt_words as u64;
    }
    /// Count every stage up to and including `last` as passed.
    pub fn pass_through(&mut self, last: &str, domain: &str, page_words: usize) {
        for s in STAGES {
            self.pass(s, domain, page_words, 0);
            if *s == last {
                break;
            }
        }
    }
    pub fn reject(&mut self, stage: &str, reason: &str, domain: &str) {
        *self
            .reasons
            .entry(stage.to_owned())
            .or_default()
            .entry(reason.to_owned())
            .or_default()
            .entry(domain.to_owned())
            .or_default() += 1;
    }
    pub fn merge(&mut self, o: &Funnel) {
        for (s, m) in &o.cells {
            for (d, c) in m {
                self.cells.entry(s.clone()).or_default().entry(d.clone()).or_default().add(c);
            }
        }
        for (s, m) in &o.reasons {
            for (r, dm) in m {
                for (d, n) in dm {
                    *self
                        .reasons
                        .entry(s.clone())
                        .or_default()
                        .entry(r.clone())
                        .or_default()
                        .entry(d.clone())
                        .or_default() += n;
                }
            }
        }
    }
    pub fn get(&self, stage: &str, domain: Option<&str>) -> Cell {
        let mut c = Cell::default();
        if let Some(m) = self.cells.get(stage) {
            for (d, x) in m {
                if domain.is_none_or(|w| w == d) {
                    c.add(x);
                }
            }
        }
        c
    }
}

/// Era bin of a capture (existence-by) date, `YYYY-MM-DD...`.
pub fn era_bin(date: &str) -> &'static str {
    let d = date.get(..10).unwrap_or("");
    if d < "2016-11-01" {
        "E0"
    } else if d < "2019-02-14" {
        "E1"
    } else if d < "2020-06-11" {
        "E2"
    } else if d < "2021-07-01" {
        "E3"
    } else if d < "2022-03-15" {
        "E4"
    } else {
        "E5"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn funnel_and_eras() {
        let mut f = Funnel::default();
        f.pass_through("url_gate", "news", 10);
        assert_eq!(f.get("records", None).docs, 1);
        assert_eq!(f.get("url_gate", Some("news")).page_words, 10);
        assert_eq!(f.get("length_gate", None).docs, 0);
        assert_eq!(era_bin("2013-05-20T00:00:00Z"), "E0");
        assert_eq!(era_bin("2019-02-14T00:00:00Z"), "E2");
        assert_eq!(era_bin("2022-11-29T23:59:59Z"), "E5");
    }
}
