//! The dry run's diagnostics (`Transferred::diag`, `Rebuilt::sources`;
//! B23): they cover every unit, agree with the transfer's own records and
//! frame lines, and read nothing a rule reads. Synthetic text only.
use grammar_a_form::conventions::{Choice, Conventions};
use grammar_a_form::layout::{Structure, rebuild_child};
use grammar_a_form::transfer::{KeptKind, SourceLayout, UnitRole, transfer};

fn none() -> Conventions {
    fn c<K>() -> Choice<K> {
        Choice { value: None, source: "none" }
    }
    Conventions { double: c(), single: c(), dash: c(), range: c(), ellipsis: c() }
}

const P1: &str = "The council met on Tuesday evening to discuss the new parking plan for the town centre. Members argued about the cost.";
const P2: &str = "Some residents said the plan would finally fix the traffic on the high street. Others were not convinced at all.";
const P3: &str = "The vote was postponed until next month when the full report will be ready for everyone to read and check.";

fn sources() -> Vec<String> {
    vec![
        format!("{P1}\n\n{P2}\n\n{P3}"),
        format!("Hi Ann,\n\n{P1}\n\n{P2}\n\n{P3}\n\n{P1}\n\nThanks,\nBo"),
        format!("{P1}\n- a waterproof coat for the rain\n- sturdy boots for the hills\n\n{P2} {P3}"),
        format!("{P1}\n\nthey also asked the office to write to every family in the village about the closures"),
        "a b\nc d\ne f\ng h".to_string(),
    ]
}

fn children() -> Vec<String> {
    vec![
        format!("{P2} {P3}\n\n{P1}"),
        format!("Hello Cy,\n\n{P3}\n\n{P2}\n\n{P1}\n\nBest,\nDee"),
        format!("{P3}\n- a warm hat for the wind\n- a flask of tea for the hills\n\n{P1}"),
        format!("{P2}\n\nthe council wrote to every family in the town about the bus changes and the parking\n\n{P3}"),
        "x y\nz w\nv u".to_string(),
    ]
}

#[test]
fn diagnostics_cover_every_unit_and_agree_with_the_records() {
    let mut kinds = std::collections::BTreeSet::new();
    for s in sources() {
        let srcs = [Some(SourceLayout::whole(&s)), SourceLayout::span(&s, 0, s.find(". ").map_or(s.len(), |i| i + 1))];
        for src in srcs.into_iter().flatten() {
            for raw in children() {
                let t = transfer(&raw, &none(), &src);
                assert_eq!(t.diag.units.len(), t.paragraphs.len(), "{raw:?}");
                // frame units are the frame lines
                let frames: Vec<(usize, usize)> =
                    t.paragraphs.iter().zip(&t.diag.units).filter(|(_, d)| matches!(d.role, UnitRole::HeadFrame | UnitRole::TailFrame)).map(|(p, _)| *p).collect();
                assert_eq!(frames, t.frame_lines);
                let kept_lines = t.diag.units.iter().filter(|d| d.role == UnitRole::KeptLine).count();
                assert_eq!(kept_lines, t.record.kept_list_lines);
                assert_eq!(t.diag.kept.iter().filter(|k| k.kind == KeptKind::ListLine).count(), t.record.kept_list_lines);
                assert_eq!(t.diag.kept.iter().filter(|k| k.kind == KeptKind::Closed).count(), t.record.closed_paragraphs);
                let wrapped = t.diag.units.iter().filter(|d| d.role == UnitRole::Body && d.wrapped).count();
                assert_eq!(wrapped, t.record.wrapped_paragraphs);
                for (p, d) in t.paragraphs.iter().zip(&t.diag.units) {
                    kinds.insert(format!("{:?}", d.role));
                    assert!(d.source.raw.0 <= d.source.raw.1 && d.source.raw.1 <= raw.len());
                    assert_eq!(d.segment.is_some(), d.role == UnitRole::Body);
                    // the unit's first word is in the raw text it draws from
                    let first = t.text[p.0..p.1].split_whitespace().next().unwrap();
                    assert!(raw[d.source.raw.0..d.source.raw.1].contains(first), "{first} {raw:?}");
                }
            }
        }
    }
    for k in ["HeadFrame", "TailFrame", "Body", "KeptLine", "Line"] {
        assert!(kinds.contains(k), "{k}: {kinds:?}");
    }
}

#[test]
fn rebuilt_paragraph_sources_name_their_lines_and_placed_breaks() {
    let t = format!("{P1}\n{P2}\n\n{P3}");
    let st = Structure { fractions: vec![0.2, 0.6], pre_words: 0, post_words: 0 };
    let r = rebuild_child(&t, &none(), &st, false);
    assert_eq!(r.sources.len(), r.paragraphs.len());
    for (p, s) in r.paragraphs.iter().zip(&r.sources) {
        let first = p.split_whitespace().next().unwrap();
        assert!(t[s.raw.0..s.raw.1].contains(first));
    }
    // breaks placed inside a line say so
    assert!(r.sources.iter().any(|s| s.starts_mid_line || s.ends_mid_line) || r.paragraphs.len() < 3, "{:?}", r.sources);
    // no sentence: no paragraph and no source
    let e = rebuild_child("   \n\n  ", &none(), &st, false);
    assert!(e.paragraphs.is_empty() && e.sources.is_empty());
}
