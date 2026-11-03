//! Excerpt-level English identification.
//!
//! An excerpt is English when all of these hold: the shared screen
//! (`english_screen`: Latin-letter share at least 0.90 and English
//! stopword share at least 0.20); pure-Rust `whatlang` says English with
//! `is_reliable`; and, when the WET record carries
//! `WARC-Identified-Content-Language` (CLD2, from CC-MAIN-2020-24), its
//! first language is `eng`. Every outcome is kept so disagreements can be
//! counted.
use serde_json::{Value, json};

pub const LANG_VERSION: &str = "cc_human_english_v1;english_screen+whatlang_0.16.4_reliable+cld2_first_eng_when_present";

#[derive(Clone, Debug)]
pub struct English {
    pub screen: Option<&'static str>,
    pub whatlang: Option<(String, f64, bool)>,
    pub cld2_first: Option<String>,
}

impl English {
    pub fn check(text: &str, cld2: Option<&str>) -> English {
        let screen = grammar_a_pools::text::english_screen(text);
        let whatlang = whatlang::detect(text).map(|i| (i.lang().code().to_owned(), i.confidence(), i.is_reliable()));
        let cld2_first = cld2.map(|c| c.split(',').next().unwrap_or("").trim().to_owned());
        English {
            screen,
            whatlang,
            cld2_first,
        }
    }
    pub fn whatlang_eng(&self) -> bool {
        matches!(&self.whatlang, Some((l, _, true)) if l == "eng")
    }
    pub fn cld2_ok(&self) -> bool {
        self.cld2_first.as_deref().is_none_or(|c| c == "eng")
    }
    /// The first failing rule, or None when the excerpt is English.
    pub fn reason(&self) -> Option<&'static str> {
        if let Some(r) = self.screen {
            return Some(r);
        }
        if !self.whatlang_eng() {
            return Some(match &self.whatlang {
                Some((l, _, _)) if l == "eng" => "whatlang_eng_not_reliable",
                _ => "whatlang_not_eng",
            });
        }
        if !self.cld2_ok() {
            return Some("cld2_first_language_not_eng");
        }
        None
    }
    /// Agreement pattern of the three rules, for disagreement counts.
    pub fn pattern(&self) -> String {
        let s = if self.screen.is_none() { "S" } else { "s" };
        let w = if self.whatlang_eng() { "W" } else { "w" };
        let c = match self.cld2_first.as_deref() {
            None => "-",
            Some("eng") => "C",
            Some(_) => "c",
        };
        format!("{s}{w}{c}")
    }
    pub fn json(&self) -> Value {
        json!({"english_screen": self.screen, "whatlang": self.whatlang.as_ref().map(|(l, c, r)| json!({"lang": l, "confidence": c, "reliable": r})),
               "cld2_first": self.cld2_first, "pattern": self.pattern()})
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn english_rules() {
        let en = "This is a plain English paragraph that talks about the weather and the town where we lived for many years, and it was a good place to be.";
        let e = English::check(en, None);
        assert_eq!(e.reason(), None, "{:?}", e);
        assert_eq!(e.pattern(), "SW-");
        assert_eq!(English::check(en, Some("fra,eng")).reason(), Some("cld2_first_language_not_eng"));
        let de = "Dies ist ein deutscher Absatz, der über das Wetter und die Stadt spricht, in der wir viele Jahre gelebt haben, und es war ein guter Ort.";
        assert!(English::check(de, None).reason().is_some());
    }
}
