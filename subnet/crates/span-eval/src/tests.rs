use super::trivial::{Geometry, Policy, Position, TrivialSet, grid};
use super::*;

/// A synthetic mixed document: `human` words of human prose, then `ai` words
/// of model prose, then `tail` human words. Returns the text and the gold
/// run's byte range.
fn mixed(human: usize, ai: usize, tail: usize) -> (String, (u32, u32)) {
    let mut t = String::new();
    for i in 0..human {
        t.push_str(&format!("hum{} ", i % 7));
        if i % 9 == 8 {
            t.push_str("end. ");
        }
    }
    let a = t.len() as u32;
    for i in 0..ai {
        if i > 0 {
            t.push(' ');
        }
        t.push_str(&format!("gen{}", i % 5));
    }
    t.push('.');
    let b = t.len() as u32;
    for i in 0..tail {
        t.push_str(&format!(" tail{}", i % 3));
    }
    (t, (a, b))
}

fn span(text: &str, a: u32, b: u32) -> Span {
    Span { start_byte: a, end_byte: b, words: words(&text[a as usize..b as usize]) }
}

fn nonspace(s: &str) -> u64 {
    s.chars().filter(|c| !is_space(*c)).count() as u64
}

#[test]
fn words_follow_the_runtime_rule() {
    assert_eq!(words(""), 0);
    assert_eq!(words("  a  bb\tc\n"), 3);
    // U+001C..U+001F separate words (Python isspace), unlike split_whitespace.
    assert_eq!(words("a\u{1c}b\u{1f}c"), 3);
    assert_eq!("a\u{1c}b".split_whitespace().count(), 1);
    assert_eq!(words("é ü\u{a0}x"), 3);
}

#[test]
fn populations_follow_the_floor() {
    let (t, g) = mixed(30, 24, 5);
    assert_eq!(population(&t, Kind::Mixed, Some(g)), Ok(Population::InScope));
    let (t2, g2) = mixed(30, 23, 5);
    assert_eq!(population(&t2, Kind::Mixed, Some(g2)), Ok(Population::Reported));
    assert_eq!(population(&t, Kind::HumanOnly, None), Ok(Population::Human));
    assert_eq!(population(&t, Kind::ModelOnly, None), Ok(Population::Reported));
    assert!(population(&t, Kind::Mixed, None).is_err());
    assert!(population(&t, Kind::Mixed, Some((0, t.len() as u32 + 1))).is_err());
    let u = "é".repeat(30);
    assert!(population(&u, Kind::Mixed, Some((1, 4))).is_err());
}

#[test]
fn exact_localization_scores_one_and_a_whole_document_span_covers_every_human_character() {
    let (t, g) = mixed(60, 40, 20);
    let exact = [span(&t, g.0, g.1)];
    let d = compute(&t, Kind::Mixed, Some(g), Emission::Spans(&exact), Some(24)).unwrap();
    assert_eq!(d.population, Population::InScope);
    assert_eq!(d.f1_ppb, PPB as u32);
    assert_eq!(d.fp_human, 0);
    let gn = nonspace(&t[g.0 as usize..g.1 as usize]);
    assert_eq!(u64::from(d.human), nonspace(&t) - gn);

    let whole = [span(&t, 0, t.len() as u32)];
    let w = compute(&t, Kind::Mixed, Some(g), Emission::Spans(&whole), Some(24)).unwrap();
    let total = nonspace(&t);
    assert_eq!(u64::from(w.f1_ppb), 2 * PPB * gn / (total + gn));
    assert_eq!(w.fp_human, w.human, "a whole-document span has C = 1");

    let none = compute(&t, Kind::Mixed, Some(g), Emission::Spans(&[]), Some(24)).unwrap();
    assert_eq!((none.f1_ppb, none.fp_human), (0, 0));
    let failed = compute(&t, Kind::Mixed, Some(g), Emission::PredictionFailed, Some(24)).unwrap();
    assert_eq!((failed.f1_ppb, failed.fp_human, failed.flagged), (0, 0, false));
}

#[test]
fn human_documents_are_flagged_by_spans_and_by_failures() {
    let t = "word ".repeat(40);
    let s = [span(&t, 0, (5 * 30 - 1) as u32)];
    for (e, flagged, emission_failed) in [
        (Emission::Spans(&[]), false, false),
        (Emission::Spans(&s), true, false),
        (Emission::PredictionFailed, true, false),
        (Emission::Failed, true, true),
        (Emission::NotEmitted, false, false),
        (Emission::Malformed, true, false),
    ] {
        let d = compute(&t, Kind::HumanOnly, None, e, Some(24)).unwrap();
        assert_eq!((d.population, d.flagged, d.emission_failed), (Population::Human, flagged, emission_failed), "{e:?}");
    }
}

#[test]
fn format_faults_are_detected() {
    let (t, g) = mixed(60, 40, 20);
    let a = span(&t, g.0, g.1);
    let first = span(&t, 0, g.0 - 1);
    let check = |s: &[Span], min| check_spans(&t, s, min);
    assert_eq!(check(&[first, a], Some(24)), None);
    assert_eq!(check(&[a, first], Some(24)), Some(FormatFault::Unsorted));
    assert_eq!(check(&[span(&t, 0, g.0 + 4), Span { start_byte: g.0, ..a }], Some(24)), Some(FormatFault::Overlap));
    assert_eq!(check(&[Span { start_byte: 3, end_byte: 3, words: 0 }], None), Some(FormatFault::Bounds));
    assert_eq!(check(&[Span { start_byte: 0, end_byte: t.len() as u32 + 1, words: 1 }], None), Some(FormatFault::Bounds));
    assert_eq!(check(&[Span { words: a.words + 1, ..a }], None), Some(FormatFault::Words));
    let short = span(&t, 0, 10);
    assert!(short.words < FLOOR_WORDS);
    assert_eq!(check(&[short], Some(24)), Some(FormatFault::UnderFloor));
    assert_eq!(check(&[short], Some(10)), None, "the floor check follows the decoder minimum");
    let u = format!("{} é tail", "x ".repeat(30));
    let at = u.find('é').unwrap() as u32 + 1;
    assert_eq!(check_spans(&u, &[Span { start_byte: 0, end_byte: at, words: 31 }], None), Some(FormatFault::CharBoundary));
    // A faulted reply counts as decoding nothing.
    let d = compute(&t, Kind::Mixed, Some(g), Emission::Spans(&[a, first]), Some(24)).unwrap();
    assert_eq!((d.format_fault, d.f1_ppb), (Some(FormatFault::Unsorted), 0));
    assert_eq!(d.flags(), 4);
}

/// Parity with the floor-24 evaluator's character rule
/// (`a_spans_evaluate.rs` `floor_metrics`): over word-aligned decoded runs,
/// `char_true` is the AI characters of the words, `char_pred` the characters
/// of the decoded words and `char_tp` the AI characters of decoded words;
/// per-document F1 is `2 tp / (pred + true)`. On spliced documents (one gold
/// run of whole words, at least 24) nothing is out of scope.
#[test]
fn character_f1_matches_the_floor_evaluator_rule_on_word_aligned_runs() {
    // xorshift64 from a fixed start: deterministic synthetic documents.
    let mut x = 88_172_645_463_325_252u64;
    let mut next = |m: u64| {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        x % m
    };
    for _ in 0..300 {
        let (t, g) = mixed(24 + next(80) as usize, 24 + next(120) as usize, next(60) as usize);
        // Words with their byte ranges.
        let mut ws: Vec<(usize, usize)> = Vec::new();
        let mut start = None;
        for (b, c) in t.char_indices() {
            match (is_space(c), start) {
                (false, None) => start = Some(b),
                (true, Some(s)) => {
                    ws.push((s, b));
                    start = None;
                }
                _ => {}
            }
        }
        if let Some(s) = start {
            ws.push((s, t.len()));
        }
        // Decoded word runs of at least 24 words.
        let mut runs = Vec::new();
        let mut w = next(10) as usize;
        while w + 24 <= ws.len() {
            let len = 24 + next(30) as usize;
            let end = (w + len).min(ws.len());
            if end - w < 24 {
                break;
            }
            runs.push((w, end));
            w = end + 1 + next(20) as usize;
        }
        let spans: Vec<Span> = runs.iter().map(|&(a, b)| span(&t, ws[a].0 as u32, ws[b - 1].1 as u32)).collect();
        let d = compute(&t, Kind::Mixed, Some(g), Emission::Spans(&spans), Some(24)).unwrap();
        // The evaluator's word-level form.
        let ai_chars = |(a, b): (usize, usize)| t[a..b].char_indices().filter(|(i, _)| ((a + i) as u32) >= g.0 && ((a + i) as u32) < g.1).count() as u64;
        let len = |(a, b): (usize, usize)| t[a..b].chars().count() as u64;
        let decoded: Vec<bool> = (0..ws.len()).map(|i| runs.iter().any(|&(a, b)| i >= a && i < b)).collect();
        let (mut tru, mut pred, mut tp) = (0u64, 0u64, 0u64);
        for (i, &wr) in ws.iter().enumerate() {
            tru += ai_chars(wr);
            if decoded[i] {
                pred += len(wr);
                tp += ai_chars(wr);
            }
        }
        assert_eq!(d.f1_ppb, f1_ppb(tp, pred, tru));
        assert_eq!(u64::from(d.fp_human), pred - tp);
        assert_eq!(u64::from(d.human), ws.iter().map(|w| len(*w)).sum::<u64>() - tru);
    }
}

fn agg(in_scope: u32, f1_sum: u64, fp: u64, human: u64, humans: u32, flagged: u32) -> Agg {
    Agg { in_scope, f1_sum, fp_human: fp, human, humans, flagged, ..Agg::default() }
}

#[test]
fn measured_conditions_hold_exactly_at_their_margins() {
    // Margins of a tenth of F1 and a twentieth of the in-context rate.
    let m = Margins { f1_ppb: PPB as u32 / 10, in_context_ppb: PPB as u32 / 20, humans: 3 };
    let anchor = agg(10, 6_000_000_000, 1_000, 100_000, 200, 5);
    // Q1: Σf1_e + Δ |M*| >= Σf1_A, at equality and one ppb below.
    assert_eq!(measured(&m, &agg(10, 5_000_000_000, 1_000, 100_000, 200, 5), &anchor), Ok(None));
    assert_eq!(measured(&m, &agg(10, 4_999_999_999, 1_000, 100_000, 200, 5), &anchor), Ok(Some(Reason::F1)));
    // QC: 10^9 Σfp_e <= 10^9 Σfp_A + Δ_C Σhuman: Δ_C Σhuman / 10^9 = 5,000 characters.
    assert_eq!(measured(&m, &agg(10, 6_000_000_000, 6_000, 100_000, 200, 5), &anchor), Ok(None));
    assert_eq!(measured(&m, &agg(10, 6_000_000_000, 6_001, 100_000, 200, 5), &anchor), Ok(Some(Reason::InContext)));
    // QH.
    assert_eq!(measured(&m, &agg(10, 6_000_000_000, 1_000, 100_000, 200, 8), &anchor), Ok(None));
    assert_eq!(measured(&m, &agg(10, 6_000_000_000, 1_000, 100_000, 200, 9), &anchor), Ok(Some(Reason::Humans)));
    // Order: Q1 before QC before QH.
    assert_eq!(measured(&m, &agg(10, 0, 99_000, 100_000, 200, 200), &anchor), Ok(Some(Reason::F1)));
    // Aggregates over different documents are not compared.
    assert_eq!(measured(&m, &agg(9, 6_000_000_000, 1_000, 100_000, 200, 5), &anchor), Err(Unpaired));
}

#[test]
fn structural_conditions_come_first() {
    let a = Agg::default();
    let m = Margins { f1_ppb: 0, in_context_ppb: 0, humans: 0 };
    let e = |token_head, decoder| Entrant { structure: Structure { token_head, decoder }, agg: &a, emission_excess: 0 };
    assert_eq!(verdict(&e(false, None), &a, Some(&m), 2, true), Ok(Some(Reason::Missing)));
    assert_eq!(verdict(&e(true, None), &a, Some(&m), 2, true), Ok(Some(Reason::Floor)));
    assert_eq!(verdict(&e(true, Some((DECODER_RULE, 23))), &a, Some(&m), 2, true), Ok(Some(Reason::Floor)));
    assert_eq!(verdict(&e(true, Some(("slop_ninja_span_decoder_v3_segment", 24))), &a, Some(&m), 2, true), Ok(Some(Reason::Floor)));
    assert_eq!(verdict(&e(true, Some((DECODER_RULE, 24))), &a, Some(&m), 2, true), Ok(None));
    // Emission failures beyond k_span, counted where the anchor emitted.
    let x = Entrant { structure: Structure { token_head: true, decoder: Some((DECODER_RULE, 24)) }, agg: &a, emission_excess: 3 };
    assert_eq!(verdict(&x, &a, Some(&m), 2, true), Ok(Some(Reason::Emission)));
    assert_eq!(verdict(&x, &a, Some(&m), 3, true), Ok(None));
    // A waived cohort applies the structural conditions only.
    assert_eq!(verdict(&x, &a, Some(&m), 2, false), Ok(None));
    assert_eq!(verdict(&e(false, None), &a, Some(&m), 2, false), Ok(Some(Reason::Missing)));
    assert_eq!(verdict(&x, &a, None, 2, true), Ok(None));
    assert_eq!(emission_excess(&[1, 4, 7, 9], &[2, 4, 9]), 2);
    assert_eq!(emission_excess(&[], &[2]), 0);
    assert_eq!(PublicValue::of(Some(Reason::Humans)), PublicValue::BelowAnchor);
    assert_eq!(PublicValue::of(Some(Reason::Floor)), PublicValue::DecoderUnderFloor);
    assert_eq!(PublicValue::of(None).code(), "passed");
    let codes: Vec<&str> = Reason::ALL.iter().map(|r| r.code()).collect();
    assert_eq!(codes, ["span.missing", "span.floor", "span.emission", "span.f1", "span.in_context", "span.humans"]);
}

#[test]
fn the_anchor_check_waives_in_order_and_never_holds() {
    let a = agg(300, 0, 0, 1, 200, 5);
    let m = Margins { f1_ppb: 1, in_context_ppb: 1, humans: 1 };
    let base = AnchorCheck {
        void: false,
        anchor: Some(&a),
        k_span: 2,
        format_faults: 0,
        trivial_passes: false,
        in_scope_min: Some(250),
        metrics_error: false,
        population_ok: true,
        margins: Some(&m),
    };
    assert_eq!(anchor_check(&base), None);
    assert_eq!(anchor_check(&AnchorCheck { void: true, metrics_error: true, ..base }), Some(Waiver::Void));
    assert_eq!(anchor_check(&AnchorCheck { anchor: None, ..base }), Some(Waiver::AnchorMissing));
    let failing = Agg { emission_failed: 3, ..a };
    assert_eq!(anchor_check(&AnchorCheck { anchor: Some(&failing), ..base }), Some(Waiver::AnchorEmission));
    assert_eq!(anchor_check(&AnchorCheck { format_faults: 1, ..base }), Some(Waiver::FormatFault));
    assert_eq!(anchor_check(&AnchorCheck { population_ok: false, ..base }), Some(Waiver::Population));
    assert_eq!(anchor_check(&AnchorCheck { margins: None, ..base }), Some(Waiver::NoMargins));
    assert_eq!(anchor_check(&AnchorCheck { trivial_passes: true, ..base }), Some(Waiver::TrivialPasses));
    assert_eq!(anchor_check(&AnchorCheck { in_scope_min: Some(301), ..base }), Some(Waiver::InScopeCount));
    assert_eq!(Waiver::Void.state(), CohortState::Void);
    assert_eq!(Waiver::NoMargins.state().code(), "waived");
}

/// Sentence byte ranges of the synthetic texts: split after `.`.
fn sentences(t: &str) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut start: Option<usize> = None;
    for (b, c) in t.char_indices() {
        if start.is_none() && !is_space(c) {
            start = Some(b);
        }
        if c == '.'
            && let Some(s) = start.take()
        {
            out.push((s, b + 1));
        }
    }
    if let Some(s) = start {
        out.push((s, t.trim_end().len()));
    }
    out
}

/// The anchor's predicted share in the trivial-policy tests.
const SHARE: u32 = 400_000_000;

#[test]
fn trivial_policies_bound_unlocalized_heads() {
    assert_eq!(grid().len(), 21 * (1 + 9 + 3));
    let (t, g) = mixed(90, 60, 30);
    let pop = population(&t, Kind::Mixed, Some(g)).unwrap();
    let sents = sentences(&t);
    let geo = Geometry::of(&t, pop, Some(g), &sents);
    let mut set = TrivialSet { policies: vec![
        Policy::Whole { tau_ppb: 0 },
        Policy::Whole { tau_ppb: 900_000_000 },
        Policy::Template { position: Position::End, percent: 10, tau_ppb: 0 },
        Policy::Sized { position: Position::Middle, tau_ppb: 0 },
    ], aggs: vec![Agg::default(); 4] };
    set.add(&geo, Some(SHARE));
    // T1(0) marks the whole document: the same numbers as a whole-document span.
    let whole = compute(&t, Kind::Mixed, Some(g), Emission::Spans(&[span(&t, 0, t.len() as u32)]), Some(24)).unwrap();
    assert_eq!(set.aggs[0].f1_sum, u64::from(whole.f1_ppb));
    assert_eq!(set.aggs[0].fp_human, u64::from(whole.fp_human));
    assert_eq!(set.aggs[0].human, u64::from(whole.human));
    // T1(0.9) does not fire at share 0.4.
    assert_eq!((set.aggs[1].f1_sum, set.aggs[1].spans), (0, 0));
    // A template run under the floor is never decoded.
    let n = sents.len();
    let k = ((n as u64 * 10 + 50) / 100).clamp(1, n as u64 - 1) as usize;
    let tail_words = words(&t[sents[n - k].0..sents[n - 1].1]);
    assert_eq!(set.aggs[2].spans, u64::from(tail_words >= FLOOR_WORDS));
    // T3 at share 0.4 covers round(0.4 n) sentences, as a span over them would.
    let k3 = ((u128::from(SHARE) * n as u128 + u128::from(PPB) / 2) / u128::from(PPB)) as usize;
    let first = (n - k3) / 2;
    let s3 = span(&t, sents[first].0 as u32, sents[first + k3 - 1].1 as u32);
    let d3 = compute(&t, Kind::Mixed, Some(g), Emission::Spans(&[s3]), Some(24)).unwrap();
    assert_eq!(set.aggs[3].f1_sum, u64::from(d3.f1_ppb));
    assert_eq!(set.aggs[3].fp_human, u64::from(d3.fp_human));

    // A human document: flagged when a policy fires or the anchor failed.
    let h = "plain words here. ".repeat(20);
    let hp = population(&h, Kind::HumanOnly, None).unwrap();
    let hgeo = Geometry::of(&h, hp, None, &sentences(&h));
    let mut hs = TrivialSet { policies: vec![Policy::Whole { tau_ppb: PPB as u32 / 2 }], aggs: vec![Agg::default()] };
    hs.add(&hgeo, Some(SHARE));
    hs.add(&hgeo, Some(PPB as u32 / 2 + SHARE / 4));
    hs.add(&hgeo, None);
    assert_eq!((hs.aggs[0].humans, hs.aggs[0].flagged), (3, 2));
}

#[test]
fn a_whole_document_policy_fails_the_in_context_condition_against_a_localizing_anchor() {
    let docs: Vec<(String, (u32, u32))> = (0..20).map(|i| mixed(60 + i, 40 + 2 * i, 10 + i)).collect();
    let mut anchor = Agg::default();
    let mut set = TrivialSet::default();
    for (t, g) in &docs {
        let s = [span(t, g.0, g.1)];
        anchor.add(&compute(t, Kind::Mixed, Some(*g), Emission::Spans(&s), Some(24)).unwrap());
        let pop = population(t, Kind::Mixed, Some(*g)).unwrap();
        set.add(&Geometry::of(t, pop, Some(*g), &sentences(t)), Some(600_000_000));
    }
    let tight = Margins { f1_ppb: PPB as u32 / 10, in_context_ppb: PPB as u32 / 20, humans: 0 };
    assert_eq!(set.any_passes(&tight, &anchor), Ok(false));
    let loose = Margins { f1_ppb: PPB as u32, in_context_ppb: PPB as u32, humans: 0 };
    assert_eq!(set.any_passes(&loose, &anchor), Ok(true));
    assert!(set.best_f1_within(PPB).is_some());
}

#[cfg(feature = "json")]
#[test]
fn replies_are_read_by_their_span_status() {
    use serde_json::json;
    let ok = json!({"status": "ok", "span_status": "ok", "prediction": {"spans": [{"start_byte": 0, "end_byte": 9, "words": 2, "start_char": 0}]}});
    assert_eq!(span_reply(&ok, true), SpanReply::Spans(vec![Span { start_byte: 0, end_byte: 9, words: 2 }]));
    assert_eq!(span_reply(&ok, false), SpanReply::PredictionFailed);
    assert_eq!(span_reply(&json!({"status": "ok"}), true), SpanReply::NotEmitted);
    assert_eq!(span_reply(&json!({"span_status": {"unsupported": "x"}}), true), SpanReply::Failed);
    assert_eq!(span_reply(&json!({"span_status": "ok", "prediction": {"spans": null}}), true), SpanReply::Malformed);
    assert_eq!(span_reply(&json!({"span_status": "ok", "prediction": {"spans": [{"start_byte": -1}]}}), true), SpanReply::Malformed);
}
