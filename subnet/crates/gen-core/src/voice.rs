//! The voice library (`eval_voice_library_v1`) and the per-document voice
//! draw.
//!
//! The library is a private JSONL object bound in the manifest by `{key,
//! sha256, bytes, version}`: a header line `{schema, version,
//! max_entry_bytes}`, then one entry per line. Every document draws its voice
//! at random (seeded by the deployment's rotation seed and the document key),
//! never from a deck, under the plan's versioned split ([`VoiceSplit`]):
//! - `profile_none_quarter_v1` (the plan default, the user's 2026-11-05
//!   decision): a quarter of all documents carry no voice profile; the
//!   other three quarters draw a profile evenly over the library, that is a
//!   category uniformly among those valid for the kind (each about 1/8 of
//!   rewrites and 3/20 of scratch documents), then an entry uniformly
//!   within it;
//! - `profile_none_half_v1`: profile or none by a fair split (half of all
//!   documents carry no voice profile and are written in the model's default
//!   voice), then a category uniformly among those valid for the kind;
//! - `profile_none_category_v1`: "no voice profile" is one outcome of the
//!   uniform draw beside the kind's categories (one in seven for a rewrite,
//!   one in six for a scratch document);
//!
//! then an entry uniformly within the category, avoiding the parent's
//! earlier voice ids. Which split a cohort uses is the plan's choice
//! (`generation.voice_split`, [`VoiceSplit::DEFAULT`] when the plan names
//! none); all three are versioned here, the two earlier ones kept for the
//! record.

use crate::hash::h64;
use crate::manifest::{Refused, VoiceLibraryRef};
use crate::vendor::DocKind;
use crate::VOICE_LIBRARY_SCHEMA;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Category {
    Persona,
    Register,
    Tone,
    Era,
    /// "Match the surrounding author's voice": rewrites only, with the same
    /// document's text around the span as read-only context.
    MatchSurrounding,
    /// The prompts real users try ("make it sound human").
    Evasion,
}

impl Category {
    pub const ALL: [Category; 6] =
        [Category::Persona, Category::Register, Category::Tone, Category::Era, Category::MatchSurrounding, Category::Evasion];
    pub fn code(self) -> &'static str {
        match self {
            Category::Persona => "persona",
            Category::Register => "register",
            Category::Tone => "tone",
            Category::Era => "era",
            Category::MatchSurrounding => "match_surrounding",
            Category::Evasion => "evasion",
        }
    }
    /// The categories a kind may draw: all six for a rewrite, all but
    /// match-surrounding for a scratch document (it has no surrounding text).
    pub fn valid_for(kind: DocKind) -> &'static [Category] {
        match kind {
            DocKind::Mixed => &Self::ALL,
            DocKind::ModelOnly => &[Category::Persona, Category::Register, Category::Tone, Category::Era, Category::Evasion],
        }
    }
}

/// The versioned share of "no voice profile" (module docs).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum VoiceSplit {
    /// No profile for 25% of documents; 75% evenly over the library.
    #[serde(rename = "profile_none_quarter_v1")]
    NoneQuarter,
    #[serde(rename = "profile_none_half_v1")]
    NoneHalf,
    #[serde(rename = "profile_none_category_v1")]
    NoneCategory,
}

impl VoiceSplit {
    pub const ALL: [VoiceSplit; 3] = [VoiceSplit::NoneQuarter, VoiceSplit::NoneHalf, VoiceSplit::NoneCategory];
    /// The split a plan uses when it names none (the user's 2026-11-05
    /// decision: no voice profile for 25% of requests).
    pub const DEFAULT: VoiceSplit = VoiceSplit::NoneQuarter;
    pub fn code(self) -> &'static str {
        match self {
            VoiceSplit::NoneQuarter => "profile_none_quarter_v1",
            VoiceSplit::NoneHalf => "profile_none_half_v1",
            VoiceSplit::NoneCategory => "profile_none_category_v1",
        }
    }
    /// The share of documents drawn with no voice profile, `(num, den)`,
    /// for a kind with `categories` valid categories.
    pub fn none_share(self, categories: usize) -> (u64, u64) {
        match self {
            VoiceSplit::NoneQuarter => (1, 4),
            VoiceSplit::NoneHalf => (1, 2),
            VoiceSplit::NoneCategory => (1, categories as u64 + 1),
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|v| v.code() == s)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Header {
    pub schema: String,
    pub version: u32,
    /// The longest entry text, in bytes (checked at build and parse).
    pub max_entry_bytes: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    pub id: String,
    pub category: Category,
    /// One line of instruction, placed first in the system prompt.
    pub text: String,
    pub kinds: Vec<DocKind>,
    /// Safe to use on a rewrite (keeps meaning, facts and names).
    pub rewrite_safe: bool,
    /// Content-free provenance (a partition or "written for v1").
    pub source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub imitated_author: Option<String>,
    /// An imitated author who is also an evaluation author (always false in
    /// a valid library: the 10-27 rule).
    pub eval_author_conflict: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Library {
    pub header: Header,
    pub entries: Vec<Entry>,
}

/// A document's voice: none (the model's default voice) or one entry.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum VoiceChoice {
    None,
    Profile { category: Category, id: String },
}

impl VoiceChoice {
    pub fn id(&self) -> Option<&str> {
        match self {
            VoiceChoice::None => None,
            VoiceChoice::Profile { id, .. } => Some(id),
        }
    }
    pub fn category(&self) -> Option<Category> {
        match self {
            VoiceChoice::None => None,
            VoiceChoice::Profile { category, .. } => Some(*category),
        }
    }
}

impl Library {
    /// Parses exactly the bound object (length and digest checked first).
    pub fn parse_bound(bytes: &[u8], r: &VoiceLibraryRef) -> Result<Self, Refused> {
        let ok = bytes.len() as u64 == r.bytes && crate::hash::sha256_hex(bytes) == r.sha256;
        if !ok {
            return Err(Refused("voice library (length or digest is not the manifest's)"));
        }
        let lib = Self::parse(bytes)?;
        if lib.header.version != r.version {
            return Err(Refused("voice library (version is not the manifest's)"));
        }
        Ok(lib)
    }

    pub fn parse(bytes: &[u8]) -> Result<Self, Refused> {
        let text = std::str::from_utf8(bytes).map_err(|_| Refused("voice library utf-8"))?;
        let mut lines = text.lines().filter(|l| !l.trim().is_empty());
        let header: Header =
            serde_json::from_str(lines.next().ok_or(Refused("voice library header"))?).map_err(|_| Refused("voice library header"))?;
        let mut entries = Vec::new();
        for l in lines {
            entries.push(serde_json::from_str::<Entry>(l).map_err(|_| Refused("voice library entry"))?);
        }
        let lib = Library { header, entries };
        lib.validate()?;
        Ok(lib)
    }

    pub fn validate(&self) -> Result<(), Refused> {
        let need = |c: bool, w: &'static str| if c { Ok(()) } else { Err(Refused(w)) };
        need(self.header.schema == VOICE_LIBRARY_SCHEMA && self.header.version >= 1, "voice library schema")?;
        let mut ids = BTreeSet::new();
        for e in &self.entries {
            need(ids.insert(e.id.as_str()) && !e.id.is_empty(), "voice library (distinct ids)")?;
            need(
                !e.text.is_empty() && !e.text.contains('\n') && e.text.len() <= self.header.max_entry_bytes as usize,
                "voice library (one line within max_entry_bytes)",
            )?;
            need(!e.kinds.is_empty() && e.kinds.iter().all(|k| Category::valid_for(*k).contains(&e.category)), "voice library (kinds)")?;
            need(!e.kinds.contains(&DocKind::Mixed) || e.rewrite_safe, "voice library (a rewrite entry is rewrite-safe)")?;
            need(!e.eval_author_conflict, "voice library (an imitated evaluation author)")?;
            need(!e.source.is_empty(), "voice library (source)")?;
        }
        for kind in [DocKind::Mixed, DocKind::ModelOnly] {
            for c in Category::valid_for(kind) {
                need(self.entries.iter().any(|e| e.category == *c && e.kinds.contains(&kind)), "voice library (every category of every kind)")?;
            }
        }
        Ok(())
    }

    pub fn entry(&self, id: &str) -> Option<&Entry> {
        self.entries.iter().find(|e| e.id == id)
    }

    /// The voice of document `doc_key` of `kind` (see the module docs).
    pub fn draw(&self, seed: &[u8; 32], doc_key: &str, kind: DocKind, avoid: &[String], split: VoiceSplit) -> VoiceChoice {
        let key = doc_key.as_bytes();
        let cats = Category::valid_for(kind);
        let category = match split {
            // `none` is one residue of four, under its own label so this
            // split's draws are independent of the other splits'.
            VoiceSplit::NoneQuarter => {
                if h64(&[seed, b"voice-quarter", key]).is_multiple_of(4) {
                    return VoiceChoice::None;
                }
                cats.get((h64(&[seed, b"voice-category", key]) % cats.len() as u64) as usize)
            }
            VoiceSplit::NoneHalf => {
                if h64(&[seed, b"voice", key]).is_multiple_of(2) {
                    return VoiceChoice::None;
                }
                cats.get((h64(&[seed, b"voice-category", key]) % cats.len() as u64) as usize)
            }
            // `none` is outcome `cats.len()` of one uniform draw.
            VoiceSplit::NoneCategory => cats.get((h64(&[seed, b"voice-category-or-none", key]) % (cats.len() as u64 + 1)) as usize),
        };
        let Some(&category) = category else {
            return VoiceChoice::None;
        };
        let pool: Vec<&Entry> = self.entries.iter().filter(|e| e.category == category && e.kinds.contains(&kind)).collect();
        if pool.is_empty() {
            return VoiceChoice::None;
        }
        let start = (h64(&[seed, b"voice-entry", key]) % pool.len() as u64) as usize;
        let n = pool.len();
        let pick = (0..n)
            .filter_map(|k| pool.get((start + k) % n))
            .find(|e| !avoid.contains(&e.id))
            .or_else(|| pool.get(start));
        match pick {
            Some(e) => VoiceChoice::Profile { category, id: e.id.clone() },
            None => VoiceChoice::None,
        }
    }
}

/// A synthetic library (two entries per category), for tests.
#[cfg(any(test, feature = "fixtures"))]
pub mod fixture {
    use super::*;

    pub fn library() -> Library {
        let mut entries = Vec::new();
        for c in Category::ALL {
            for i in 0..2 {
                let kinds = if c == Category::MatchSurrounding { vec![DocKind::Mixed] } else { vec![DocKind::Mixed, DocKind::ModelOnly] };
                entries.push(Entry {
                    id: format!("{}_{i}", c.code()),
                    category: c,
                    text: format!("TEST voice {} {i}.", c.code()),
                    kinds,
                    rewrite_safe: true,
                    source: "synthetic".into(),
                    imitated_author: None,
                    eval_author_conflict: false,
                });
            }
        }
        Library { header: Header { schema: VOICE_LIBRARY_SCHEMA.into(), version: 1, max_entry_bytes: 256 }, entries }
    }

    pub fn to_jsonl(l: &Library) -> Vec<u8> {
        let mut out = serde_json::to_string(&l.header).unwrap_or_default();
        for e in &l.entries {
            out.push('\n');
            out.push_str(&serde_json::to_string(e).unwrap_or_default());
        }
        out.push('\n');
        out.into_bytes()
    }

    pub fn reference(l: &Library) -> VoiceLibraryRef {
        let b = to_jsonl(l);
        VoiceLibraryRef {
            key: "generation/voices/voice-library-v1.jsonl".into(),
            sha256: crate::hash::sha256_hex(&b),
            bytes: b.len() as u64,
            version: l.header.version,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fixture::*;
    use super::*;

    #[test]
    fn the_library_round_trips_and_is_bound() {
        let l = library();
        let b = to_jsonl(&l);
        let r = reference(&l);
        assert_eq!(Library::parse_bound(&b, &r).unwrap(), l);
        let mut bad = r.clone();
        bad.bytes += 1;
        assert!(Library::parse_bound(&b, &bad).is_err());
    }

    #[test]
    fn invalid_entries_are_refused() {
        let mut l = library();
        l.entries[0].text = "two\nlines".into();
        assert!(l.validate().is_err());
        let mut l = library();
        l.entries.iter_mut().find(|e| e.category == Category::MatchSurrounding).unwrap().kinds.push(DocKind::ModelOnly);
        assert_eq!(l.validate().unwrap_err().0, "voice library (kinds)");
        let mut l = library();
        l.entries[0].eval_author_conflict = true;
        assert!(l.validate().is_err());
        let mut l = library();
        l.entries.retain(|e| e.category != Category::Evasion);
        assert_eq!(l.validate().unwrap_err().0, "voice library (every category of every kind)");
    }

    #[test]
    fn half_carry_no_profile_and_categories_spread() {
        let l = library();
        let seed = [9u8; 32];
        for kind in [DocKind::Mixed, DocKind::ModelOnly] {
            let mut none: usize = 0;
            let mut cats = std::collections::BTreeMap::new();
            let n: usize = 6000;
            for i in 0..n {
                match l.draw(&seed, &format!("doc-{i}"), kind, &[], VoiceSplit::NoneHalf) {
                    VoiceChoice::None => none += 1,
                    VoiceChoice::Profile { category, id } => {
                        assert!(Category::valid_for(kind).contains(&category));
                        assert_eq!(l.entry(&id).unwrap().category, category);
                        *cats.entry(category).or_insert(0usize) += 1;
                    }
                }
            }
            assert!((n / 2 - 200..n / 2 + 200).contains(&none), "{none}");
            assert_eq!(cats.len(), Category::valid_for(kind).len());
            let per = (n - none) / cats.len();
            assert!(cats.values().all(|c| c.abs_diff(per) < per / 4), "{cats:?}");
            assert_eq!(kind == DocKind::ModelOnly, !cats.contains_key(&Category::MatchSurrounding));
        }
        // Determinism, and avoidance of the parent's earlier voice ids.
        let a = l.draw(&seed, "doc-1", DocKind::Mixed, &[], VoiceSplit::NoneHalf);
        assert_eq!(a, l.draw(&seed, "doc-1", DocKind::Mixed, &[], VoiceSplit::NoneHalf));
        for i in 0..200 {
            let k = format!("doc-{i}");
            if let VoiceChoice::Profile { category, id } = l.draw(&seed, &k, DocKind::Mixed, &[], VoiceSplit::NoneHalf) {
                let again = l.draw(&seed, &k, DocKind::Mixed, std::slice::from_ref(&id), VoiceSplit::NoneHalf);
                assert_eq!(again.category(), Some(category));
                assert_ne!(again.id(), Some(id.as_str()));
            }
        }
    }

    #[test]
    fn none_as_one_category_is_one_outcome_of_the_uniform_draw() {
        let l = library();
        let seed = [11u8; 32];
        for kind in [DocKind::Mixed, DocKind::ModelOnly] {
            let outcomes = Category::valid_for(kind).len() + 1;
            let n: usize = 7000;
            let mut none = 0usize;
            let mut cats = std::collections::BTreeMap::new();
            for i in 0..n {
                match l.draw(&seed, &format!("doc-{i}"), kind, &[], VoiceSplit::NoneCategory) {
                    VoiceChoice::None => none += 1,
                    VoiceChoice::Profile { category, .. } => *cats.entry(category).or_insert(0usize) += 1,
                }
            }
            let per = n / outcomes;
            assert!(none.abs_diff(per) < per / 5, "{kind:?} none {none} of {n}, expected about {per}");
            assert_eq!(cats.len(), outcomes - 1);
            assert!(cats.values().all(|c| c.abs_diff(per) < per / 5), "{cats:?}");
        }
        for v in VoiceSplit::ALL {
            assert_eq!(VoiceSplit::parse(v.code()), Some(v));
            assert_eq!(serde_json::to_value(v).unwrap(), v.code());
        }
    }

    #[test]
    fn a_quarter_carry_no_profile_and_the_rest_spread_evenly_over_the_library() {
        let l = library();
        let seed = [13u8; 32];
        assert_eq!(VoiceSplit::DEFAULT, VoiceSplit::NoneQuarter);
        assert_eq!(VoiceSplit::DEFAULT.code(), "profile_none_quarter_v1");
        for kind in [DocKind::Mixed, DocKind::ModelOnly] {
            let n: usize = 8000;
            let (mut none, mut cats, mut ids) = (0usize, std::collections::BTreeMap::new(), std::collections::BTreeMap::new());
            for i in 0..n {
                match l.draw(&seed, &format!("doc-{i}"), kind, &[], VoiceSplit::NoneQuarter) {
                    VoiceChoice::None => none += 1,
                    VoiceChoice::Profile { category, id } => {
                        assert!(Category::valid_for(kind).contains(&category));
                        *cats.entry(category).or_insert(0usize) += 1;
                        *ids.entry(id).or_insert(0usize) += 1;
                    }
                }
            }
            let (num, den) = VoiceSplit::NoneQuarter.none_share(Category::valid_for(kind).len());
            let want_none = n * num as usize / den as usize;
            assert!(none.abs_diff(want_none) < want_none / 10, "{kind:?}: none {none}, expected about {want_none}");
            // Three quarters over the kind's categories, then evenly within each.
            assert_eq!(cats.len(), Category::valid_for(kind).len());
            let per = (n - none) / cats.len();
            assert!(cats.values().all(|c| c.abs_diff(per) < per / 6), "{cats:?}");
            let per_id = (n - none) / ids.len();
            assert!(ids.values().all(|c| c.abs_diff(per_id) < per_id / 4), "{ids:?}");
        }
        // Deterministic, and avoidance still re-draws within the category.
        for i in 0..200 {
            let k = format!("doc-{i}");
            let a = l.draw(&seed, &k, DocKind::Mixed, &[], VoiceSplit::NoneQuarter);
            assert_eq!(a, l.draw(&seed, &k, DocKind::Mixed, &[], VoiceSplit::NoneQuarter));
            if let VoiceChoice::Profile { category, id } = a {
                let again = l.draw(&seed, &k, DocKind::Mixed, std::slice::from_ref(&id), VoiceSplit::NoneQuarter);
                assert_eq!(again.category(), Some(category));
                assert_ne!(again.id(), Some(id.as_str()));
            }
        }
        assert_eq!(VoiceSplit::NoneHalf.none_share(6), (1, 2));
        assert_eq!(VoiceSplit::NoneCategory.none_share(6), (1, 7));
    }
}
