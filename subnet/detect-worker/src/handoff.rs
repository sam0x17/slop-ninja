//! The seam between the free check and the store: a result as the answer
//! line carried it ([`ResultFull`], also what a result seal holds) becomes
//! the store's [`StoredResult`], and an opened result seal the store's
//! [`Sealed`].
//!
//! The answer line prints what the page shows; the store keeps the same
//! values, typed by the page contract, with the run's record: when it was
//! scored, the package that scored it (`package_load`, its label and
//! published digest), the scorer build, the result policy and the evidence
//! table. The label, build and evidence are the serving deployment's, so a
//! result converts only under the package that scored it: a seal issued
//! before a package switch answers `seal_invalid`, and the page asks for a
//! new check. Every value a seal of this Worker carries converts; anything
//! else is refused, never repaired.

use slop_ninja_detect_page::codes::{Band, CutKind, LengthBin, ShortResult, SpansNote};
use slop_ninja_detect_page::page::Span;
use slop_ninja_detect_page::refusal::RefusalCode;

use crate::answer::{self, ResultFull};
use crate::doc::{Serving, StoredResult};
use crate::seal::{Opened, ResultPayload};
use crate::share::Sealed;

/// A result that does not convert (another package, or a value no answer
/// line carries).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Mismatch;

fn u32_of(n: impl TryInto<u32>) -> Result<u32, Mismatch> {
    n.try_into().map_err(|_| Mismatch)
}

fn note_of(n: answer::SpansNote) -> SpansNote {
    match n {
        answer::SpansNote::StrongBandsOnly => SpansNote::StrongBandsOnly,
        answer::SpansNote::Off => SpansNote::Off,
        answer::SpansNote::Unavailable => SpansNote::Unavailable,
    }
}

/// The band of a text of 150 words and more, or a short text's result.
fn band_of(band: &str, length: LengthBin) -> Result<(Option<Band>, Option<ShortResult>), Mismatch> {
    if length.is_short() {
        return match band {
            "strong" => Ok((None, Some(ShortResult::Strong))),
            "no_strong_signs" => Ok((None, Some(ShortResult::None))),
            _ => Err(Mismatch),
        };
    }
    Band::parse(band).map(|b| (Some(b), None)).ok_or(Mismatch)
}

/// The store's record of a result scored at `scored_at` by `package_load`
/// under `policy`, while `serving` serves.
pub fn stored_result(
    full: &ResultFull,
    scored_at: u64,
    package_load: &str,
    policy: &str,
    serving: &Serving,
) -> Result<StoredResult, Mismatch> {
    if package_load != serving.load || full.package.digest != serving.published {
        return Err(Mismatch);
    }
    let length = LengthBin::parse(&full.length).ok_or(Mismatch)?;
    let (band, short) = band_of(&full.band, length)?;
    let spans = full
        .spans
        .as_deref()
        .unwrap_or_default()
        .iter()
        .map(|s| {
            Ok(Span {
                start: u32_of(s.start)?,
                end: u32_of(s.end)?,
                words: u32_of(s.words)?,
                confidence: s.confidence.map(|c| u8::try_from(c.min(100)).unwrap_or(100)),
            })
        })
        .collect::<Result<Vec<_>, Mismatch>>()?;
    // What the page contract requires of a shown result (`Page::check`): a
    // banded result prints its figures, a short one none (and its passages
    // no confidence); passages only in the strong results; the scored words
    // within the text, all of them unless it was cut.
    let numbers = [
        full.confidence_tenths.is_some(),
        full.confidence_text.is_some(),
        full.share_text.is_some(),
        full.share_pct.is_some(),
    ];
    let shown = if short.is_some() {
        numbers.iter().all(|n| !n) && spans.iter().all(|s| s.confidence.is_none())
    } else {
        numbers.iter().all(|n| *n) && full.share_pct.is_some_and(|p| p <= 100)
    };
    let strong = band.is_some_and(Band::is_strong) || short == Some(ShortResult::Strong);
    let cut = CutKind::parse(&full.cut).ok_or(Mismatch)?;
    let words_ok = full.words <= full.total_words && ((cut == CutKind::None) == (full.words == full.total_words));
    if !shown || !(strong || spans.is_empty()) || !words_ok {
        return Err(Mismatch);
    }
    Ok(StoredResult {
        scored_at,
        package_load: package_load.to_owned(),
        package_label: serving.label.clone(),
        package_pub: full.package.digest.clone(),
        publication_pending: full.package.publication_pending,
        scorer_build: serving.scorer_build.clone(),
        policy: policy.to_owned(),
        evidence: serving.evidence.clone(),
        band,
        short,
        confidence_tenths: full.confidence_tenths,
        confidence_text: full.confidence_text.clone(),
        share_pct: full.share_pct,
        share_text: full.share_text.clone(),
        words: u32_of(full.words)?,
        total_words: u32_of(full.total_words)?,
        cut,
        tokens: u32_of(full.tokens)?,
        length: Some(length),
        spans,
        spans_note: full.spans_note.map(note_of),
        markers: full.markers.clone(),
        invisible: full.invisible,
        raw: None,
    })
}

/// An opened result seal as the store takes it (`/share`, `/preview`).
/// `seal_invalid` when its result does not convert under `serving`.
pub fn sealed(o: Opened<ResultPayload>, serving: &Serving) -> Result<Sealed, RefusalCode> {
    let p = o.payload;
    let mut text_sha256 = [0u8; 32];
    hex::decode_to_slice(&p.text_sha256, &mut text_sha256).map_err(|_| RefusalCode::SealInvalid)?;
    let result = stored_result(&p.result_full, p.issued_ms, &p.package_load, &p.policy_v, serving)
        .map_err(|_| RefusalCode::SealInvalid)?;
    Ok(Sealed { text_sha256, nonce: o.nonce.to_vec(), result })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::answer::{PackageLine, Span as LineSpan};

    const PUBLISHED: &str = "e2536d12dff593d7b11895b3cbf6b10232601abfbebde248325afd707722b5e8";

    fn serving() -> Serving {
        Serving {
            load: "a".repeat(64),
            label: "span package d24".into(),
            published: PUBLISHED.into(),
            pending: true,
            scorer_build: "7030a86f".into(),
            evidence: "evidence-d24-web-v1".into(),
        }
    }

    fn full(band: &str, length: &str) -> ResultFull {
        ResultFull {
            band: band.into(),
            confidence_tenths: Some(999),
            confidence_text: Some("above 99.9%".into()),
            share_text: Some("about 86%".into()),
            share_pct: Some(86),
            words: 300,
            total_words: 1200,
            cut: "first_1000_words".into(),
            scored_end: 1800,
            tokens: 400,
            length: length.into(),
            spans: Some(vec![LineSpan { start: 10, end: 300, words: 40, confidence: Some(93) }]),
            spans_note: None,
            markers: vec!["[CLS]".into()],
            invisible: true,
            package: PackageLine { digest: PUBLISHED.into(), publication_pending: true },
        }
    }

    #[test]
    fn a_long_result_keeps_every_printed_value() {
        let s = serving();
        let r = stored_result(&full("very_strong", "150_450"), 5, &s.load, "long-v1", &s).unwrap();
        assert_eq!((r.band, r.short), (Some(Band::VeryStrong), None));
        assert_eq!((r.confidence_tenths, r.share_pct), (Some(999), Some(86)));
        assert_eq!(r.spans, vec![Span { start: 10, end: 300, words: 40, confidence: Some(93) }]);
        assert_eq!((r.words, r.total_words, r.tokens, r.cut), (300, 1200, 400, CutKind::FirstWords));
        assert_eq!((r.length, r.markers.len(), r.invisible), (Some(LengthBin::Measured), 1, true));
        assert_eq!((r.package_label.as_str(), r.scorer_build.as_str()), ("span package d24", "7030a86f"));
        assert_eq!((r.policy.as_str(), r.evidence.as_str(), r.scored_at), ("long-v1", "evidence-d24-web-v1", 5));
        assert_eq!(r.package_pub, PUBLISHED);
        assert!(r.publication_pending && r.raw.is_none());
    }

    /// A short text's line: no percentages, passages without confidence.
    fn short(band: &str, length: &str) -> ResultFull {
        let mut f = full(band, length);
        (f.confidence_tenths, f.confidence_text, f.share_text, f.share_pct) = (None, None, None, None);
        for s in f.spans.iter_mut().flatten() {
            s.confidence = None;
        }
        if band != "strong" {
            f.spans = None;
            f.spans_note = Some(answer::SpansNote::StrongBandsOnly);
        }
        f
    }

    #[test]
    fn a_short_result_is_the_short_rule_s() {
        let s = serving();
        let r = stored_result(&short("strong", "50_99"), 5, &s.load, "long-v1", &s).unwrap();
        assert_eq!((r.band, r.short), (None, Some(ShortResult::Strong)));
        assert_eq!(r.spans[0].confidence, None);
        let r = stored_result(&short("no_strong_signs", "24_49"), 5, &s.load, "long-v1", &s).unwrap();
        assert_eq!((r.band, r.short, r.spans_note), (None, Some(ShortResult::None), Some(SpansNote::StrongBandsOnly)));
        // A band the length cannot have.
        assert_eq!(stored_result(&short("very_strong", "24_49"), 5, &s.load, "x", &s), Err(Mismatch));
        assert_eq!(stored_result(&full("no_strong_signs", "150_450"), 5, &s.load, "x", &s), Err(Mismatch));
    }

    #[test]
    fn a_result_the_page_could_not_show_is_refused() {
        let s = serving();
        let ok = |f: &ResultFull| stored_result(f, 5, &s.load, "long-v1", &s).is_ok();
        assert!(ok(&full("strong", "150_450")) && ok(&short("strong", "24_49")));
        // A short text that prints a percentage, or a passage confidence.
        assert!(!ok(&full("strong", "24_49")));
        let mut f = short("strong", "24_49");
        f.spans.as_mut().unwrap()[0].confidence = Some(90);
        assert!(!ok(&f));
        // A banded text without its figures.
        let mut f = full("strong", "150_450");
        f.share_pct = None;
        assert!(!ok(&f));
        // Passages outside the strong results.
        assert!(!ok(&full("unclear", "150_450")));
        // Scored words past the text, or a whole text that says it was cut.
        let mut f = full("strong", "150_450");
        f.words = f.total_words + 1;
        assert!(!ok(&f));
        let mut f = full("strong", "150_450");
        f.words = f.total_words;
        assert!(!ok(&f));
        f.cut = "none".into();
        assert!(ok(&f));
    }

    #[test]
    fn only_the_package_that_scored_it_converts_it() {
        let s = serving();
        assert_eq!(stored_result(&full("strong", "150_450"), 5, &"b".repeat(64), "long-v1", &s), Err(Mismatch));
        let mut f = full("strong", "150_450");
        f.package.digest = "f".repeat(64);
        assert_eq!(stored_result(&f, 5, &s.load, "long-v1", &s), Err(Mismatch));
        for bad in [("cut", "all"), ("length", "1_23")] {
            let mut f = full("strong", "150_450");
            if bad.0 == "cut" {
                f.cut = bad.1.into();
            } else {
                f.length = bad.1.into();
            }
            assert_eq!(stored_result(&f, 5, &s.load, "long-v1", &s), Err(Mismatch), "{bad:?}");
        }
    }
}
