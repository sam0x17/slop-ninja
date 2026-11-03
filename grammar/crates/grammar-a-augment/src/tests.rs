use super::*;

/// Synthetic prose with contractions, articles, commas, OCR-prone letters,
/// list words and line breaks (written for these tests).
fn prose(k: usize) -> String {
    let base = [
        "The big dog didn't start the long journey, but the clever child often ran down the road.",
        "Maybe the small shop on the street will open early; it's hard to choose a gift for kids.",
        "An old modern rock wall stood near the corner, and nearly every child was afraid of it.",
        "We talked about the error at length:\nnobody thought it would be difficult to fix quickly.",
    ];
    (0..k).map(|i| base[i % base.len()]).collect::<Vec<_>>().join(if k.is_multiple_of(2) { " " } else { "\n\n" })
}

fn heavy() -> Profile {
    let mut p = Profile::builtin("train_v1").unwrap();
    p.name = "test_heavy".into();
    p.intensity_min = 2.0;
    p.intensity_max = 2.0;
    p.max_edit_fraction = 0.5;
    for v in p.per_100_words.values_mut() {
        *v = (*v * 2.0).min(20.0);
    }
    p.validate().unwrap();
    p
}

fn row(id: &str, text: &str, origin: &str, spans: &[(usize, usize)]) -> Value {
    let (h, m) = class_words(text, spans).unwrap();
    let share = if h + m == 0 { 0.0 } else { m as f64 / (h + m) as f64 };
    json!({"id": id, "split": "train", "origin": origin, "share": share, "text": text,
        "text_sha256": hex::encode(Sha256::digest(text.as_bytes())),
        "ai_byte_spans": spans.iter().map(|&(s, e)| [s, e]).collect::<Vec<_>>(),
        "source_group": format!("g-{id}"), "writer_key": "w", "domain": "fineweb", "population": "primary", "word_count": h + m})
}

/// A mixed layout: model span over the middle sentence(s).
fn mixed(text: &str) -> Vec<(usize, usize)> {
    let a = text.find("Maybe").unwrap();
    let b = text.find("of it.").unwrap() + "of it.".len();
    vec![(a, b)]
}

#[test]
fn deterministic_and_seed_sensitive() {
    let t = prose(8);
    let p = heavy();
    let a = augment(&t, &[], &p, 7, "r1").unwrap();
    let b = augment(&t, &[], &p, 7, "r1").unwrap();
    assert_eq!(a.text, b.text);
    assert_eq!(a.record, b.record);
    assert_ne!(a.text, t);
    assert_ne!(augment(&t, &[], &p, 8, "r1").unwrap().text, a.text);
    assert_ne!(augment(&t, &[], &p, 7, "r2").unwrap().text, a.text);
}

#[test]
fn label_blind_same_text_same_edits() {
    // Human-only and model-only labellings form one run each, so even the
    // deletion guard sees the same thing: outputs must be byte-identical.
    let p = heavy();
    for k in 1..12 {
        let t = prose(k);
        for seed in 0..5 {
            let h = augment(&t, &[], &p, seed, "x").unwrap();
            let m = augment(&t, &[(0, t.len())], &p, seed, "x").unwrap();
            assert_eq!(h.text, m.text, "k={k} seed={seed}");
            assert_eq!(h.record["edits"].as_array().unwrap().len(), m.record["edits"].as_array().unwrap().len());
            assert_eq!(m.spans, vec![(0, m.text.len())]);
            assert!(h.spans.is_empty());
            // mixed: identical unless the run guard intervened
            if k < 3 {
                continue;
            }
            let mx = augment(&t, &mixed(&t), &p, seed, "x");
            if let Ok(mx) = mx
                && mx.record["deletions_skipped_by_run_guard"] == 0
                && h.record["deletions_skipped_by_run_guard"] == 0
            {
                assert_eq!(mx.text, h.text);
            }
        }
    }
}

#[test]
fn spans_remap_exactly_and_origin_is_kept() {
    let p = heavy();
    let mut saw_delete = false;
    for k in [3usize, 4, 6, 9] {
        let t = prose(k);
        let sp = mixed(&t);
        let src = word_labels(&t, &sp).unwrap();
        for seed in 0..40 {
            let a = augment(&t, &sp, &p, seed, "m").unwrap();
            let got = word_labels(&a.text, &a.spans).unwrap();
            // labels of the kept source words, in order
            let edits = a.record["edits"].as_array().unwrap();
            let del: std::collections::BTreeSet<usize> = edits
                .iter()
                .filter(|e| e[2] == "delete")
                .map(|e| e[0].as_u64().unwrap() as usize)
                .collect();
            saw_delete |= !del.is_empty();
            let want: Vec<u8> = src.iter().enumerate().filter(|(i, _)| !del.contains(i)).map(|(_, w)| w.1).collect();
            assert_eq!(got.iter().map(|w| w.1).collect::<Vec<_>>(), want);
            assert_eq!(a.words, want.len());
            assert_eq!(a.model_words, want.iter().filter(|x| **x == 1).count());
            assert!(a.model_words > 0 && a.model_words < a.words, "mixed stays mixed");
            assert_eq!(a.record["words_after"], a.words);
            // deletions and edits respect the cap
            assert!(a.record["word_edits"].as_u64().unwrap() as usize <= a.record["edit_cap"].as_u64().unwrap() as usize);
        }
    }
    assert!(saw_delete);
}

#[test]
fn guard_keeps_single_word_runs() {
    let t = "the the the a the an the";
    let mut p = Profile::builtin("train_v1").unwrap();
    p.per_100_words = [("article_drop".to_string(), 50.0)].into_iter().collect();
    p.intensity_min = 2.0;
    p.intensity_max = 2.0;
    p.max_edit_fraction = 1.0;
    p.name = "drops".into();
    // model span = the single word "a"
    let s = t.find(" a ").unwrap() + 1;
    for seed in 0..30 {
        let a = augment(t, &[(s, s + 1)], &p, seed, "g").unwrap();
        assert_eq!(a.model_words, 1);
        assert!(a.words >= 2);
        let h = augment(t, &[], &p, seed, "g").unwrap();
        assert!(h.words >= 1);
    }
}

#[test]
fn ops_change_only_word_internals() {
    // every word-level op keeps whitespace and word boundaries
    let p = heavy();
    let t = prose(16);
    let mut ops = std::collections::BTreeSet::new();
    for seed in 0..12 {
        let a = augment(&t, &[], &p, seed, "ops").unwrap();
        ops.extend(a.record["edits_by_operation"].as_object().unwrap().keys().cloned());
    }
    let a = augment(&t, &[], &p, 3, "ops").unwrap();
    for o in ["typo", "casing", "punct", "whitespace", "article_drop", "apostrophe"] {
        assert!(ops.contains(o), "missing {o} in {ops:?}");
    }
    assert_eq!(a.record["words_before"].as_u64().unwrap() as usize - a.words, a.record["edits"].as_array().unwrap().iter().filter(|e| e[2] == "delete").count());
    // zero-width characters never split a word
    for z in ZERO_WIDTH {
        assert!(!z.is_whitespace());
    }
    assert!('\u{a0}'.is_whitespace());
}

#[test]
fn single_ops_behave() {
    let mut r = Rng::keyed(&["t"]);
    assert_eq!(apply("synonym", "Big,", &mut r).0, "Large,");
    assert_eq!(apply("synonym", "BIG", &mut r).0, "LARGE");
    assert!(!applicable("synonym", "bIg"));
    assert_eq!(apply("apostrophe", "didn't", &mut r).0, "didnt");
    assert_eq!(apply("apostrophe", "it\u{2019}s.", &mut r).0, "its.");
    assert_eq!(apply("punct", "road,", &mut r).0, "road");
    assert!(!applicable("punct", "road."));
    assert!(applicable("article_drop", "The") && !applicable("article_drop", "the,"));
    assert!(!applicable("word_drop", "end."));
    let (w, v) = apply("homoglyph", "cat", &mut r);
    assert_ne!(w, "cat");
    assert_eq!(w.chars().count(), 3);
    assert!(v.contains("->U+"));
    let (w, _) = apply("zero_width", "word", &mut r);
    assert_eq!(w.chars().filter(|c| !ZERO_WIDTH.contains(c)).collect::<String>(), "word");
    assert_eq!(w.split_whitespace().count(), 1);
    for _ in 0..50 {
        let (w, _) = apply("typo", "modern", &mut r);
        assert_ne!(w, "modern");
        assert!(!w.contains(char::is_whitespace));
        let (o, _) = apply("ocr", "modern", &mut r);
        assert_ne!(o, "modern");
    }
    assert_eq!(split_core("\"(Hello!)\""), ("\"(", "Hello", "!)\""));
    assert_eq!(split_core("--"), ("--", "", ""));
}

#[test]
fn rows_keep_metadata_and_validate_inputs() {
    let p = Profile::builtin("train_v1").unwrap();
    let t = prose(6);
    let r = row("doc1", &t, "mixed", &mixed(&t));
    let a = augment_row(&r, &p, 11).unwrap();
    assert_eq!(a["id"], "doc1#aug:train_v1:11");
    assert_eq!(a["augmented_from"], "doc1");
    for k in ["split", "source_group", "writer_key", "domain", "population", "origin"] {
        assert_eq!(a[k], r[k]);
    }
    let (_, origin, spans) = row_parts(&a).unwrap();
    assert_eq!(origin, "mixed");
    let (h, m) = class_words(a["text"].as_str().unwrap(), &spans).unwrap();
    assert_eq!(a["word_count"], h + m);
    assert!((a["share"].as_f64().unwrap() - m as f64 / (h + m) as f64).abs() < 1e-12);
    // model-only keeps share 1 and a whole-text span
    let mo = row("doc2", &t, "model_only", &[(0, t.len())]);
    let b = augment_row(&mo, &p, 11).unwrap();
    assert_eq!(b["share"], 1.0);
    assert_eq!(b["ai_byte_spans"], json!([[0, b["text"].as_str().unwrap().len()]]));
    // inconsistent inputs are refused
    let mut bad = r.clone();
    bad["share"] = json!(0.5);
    assert!(augment_row(&bad, &p, 1).is_err());
    let mut bad = r.clone();
    bad["origin"] = json!("human_only");
    assert!(augment_row(&bad, &p, 1).is_err());
    let mut bad = r.clone();
    bad["text_sha256"] = json!("00");
    assert!(augment_row(&bad, &p, 1).is_err());
    let mut bad = r.clone();
    bad["ai_byte_spans"] = json!([[1, 5]]);
    assert!(augment_row(&bad, &p, 1).is_err());
}

#[test]
fn selection_is_class_blind_and_near_rate() {
    let n = 4000;
    let picked = (0..n).filter(|i| selected("copy", 5, &format!("row{i}"), 0.25)).count();
    assert!((picked as f64 / n as f64 - 0.25).abs() < 0.03, "{picked}");
    // the draw depends on the id only, never on labels
    assert_eq!(select_unit("copy", 5, "row1"), select_unit("copy", 5, "row1"));
    assert_ne!(select_unit("copy", 5, "row1"), select_unit("probe_repeat", 5, "row1"));
}

#[test]
fn audit_rates_match_across_classes() {
    // the same texts labelled human and model give identical per-class rates
    let p = Profile::builtin("train_v1").unwrap();
    let mut au = Audit::default();
    for i in 0..60 {
        let t = prose(4 + i % 5);
        let id = format!("d{i}");
        let h = row(&id, &t, "human_only", &[]);
        let m = row(&id, &t, "model_only", &[(0, t.len())]);
        for r in [&h, &m] {
            let (_, origin, spans) = row_parts(r).unwrap();
            let a = augment_row(r, &p, 2).unwrap();
            au.add(origin, class_words(&t, &spans).unwrap(), Some(&a["augmentation"]));
        }
    }
    let rep = au.report();
    assert_eq!(rep["augmented_human_words"], rep["augmented_model_words"]);
    for (op, v) in rep["edits_by_operation"].as_object().unwrap() {
        if op != GAP_OP {
            assert_eq!(v["human_words_edited"], v["model_words_edited"], "{op}");
        }
    }
}

#[test]
fn probes_repeat_and_perturb() {
    let p = Profile::builtin("light_v1").unwrap();
    let t = prose(12);
    let r = row("e1", &t, "mixed", &mixed(&t));
    let v = probes_for_row(&r, &p, 3, 1.0, 1.0).unwrap();
    assert_eq!(v.len(), 2);
    let (m0, r0) = &v[0];
    assert_eq!(m0["schema"], PROBE_SCHEMA);
    assert_eq!(m0["kind"], "repeat");
    assert_eq!(r0["text"], r["text"]);
    assert_eq!(m0["text_sha256"], m0["base_text_sha256"]);
    let (m1, r1) = &v[1];
    assert_eq!(m1["kind"], "perturbed");
    assert_eq!(r1["id"], m1["probe_id"]);
    assert_eq!(r1["probe_of"], "e1");
    assert!(m1.get("text").is_none() && m0.get("text").is_none(), "manifest carries no text");
    assert_eq!(m1["text_changed"], json!(m1["text_sha256"] != m1["base_text_sha256"]));
    let (_, origin, _) = row_parts(r1).unwrap();
    assert_eq!(origin, "mixed");
    // light: few edits
    let words = t.split_whitespace().count() as u64;
    assert!(m1["word_edits"].as_u64().unwrap() <= (words * 3).div_ceil(100).max(1));
    assert!(probes_for_row(&r, &p, 3, 0.0, 0.0).unwrap().is_empty());
}

#[test]
fn min_edits_changes_short_texts_label_blind() {
    let p = Profile::builtin("light_v1").unwrap();
    let t = "We walked home along the quiet road before dinner.";
    for seed in 0..60 {
        let h = augment(t, &[], &p, seed, "short").unwrap();
        let m = augment(t, &[(0, t.len())], &p, seed, "short").unwrap();
        assert_ne!(h.text, t, "seed {seed}");
        assert_eq!(h.text, m.text);
        assert!(h.record["word_edits"].as_u64().unwrap() >= 1);
        assert_eq!(h.words, 9);
    }
    // nothing editable: no forced edit, no error
    let only_digits = "1 2 3";
    let a = augment(only_digits, &[], &p, 1, "d").unwrap();
    assert_eq!(a.record["forced_edits"], 0);
}

#[test]
fn profiles_validate() {
    for n in ["train_v1", "light_v1"] {
        let p = Profile::builtin(n).unwrap();
        assert_eq!(p.sha256(), Profile::builtin(n).unwrap().sha256());
    }
    assert!(Profile::builtin("nope").is_err());
    let mut p = Profile::builtin("train_v1").unwrap();
    p.per_100_words.insert("teleport".into(), 1.0);
    assert!(p.validate().is_err());
    let mut p = Profile::builtin("train_v1").unwrap();
    p.per_100_words.insert("typo".into(), 90.0);
    assert!(p.validate().is_err());
    let light = Profile::builtin("light_v1").unwrap();
    for o in ["article_drop", "word_drop", "synonym", "homoglyph", "ocr"] {
        assert!(!light.per_100_words.contains_key(o));
    }
}

// ------------------------------------------------------------ gen_form_inject_v1

mod form_inject_tests {
    use super::row;
    use crate::form_inject::*;
    use crate::{row_parts, word_labels};
    use grammar_a_form::conventions::{Changes, Choice, Conventions, DASHES, Dash, ELLIPSES, Ellipsis, Quote, render_quotes, render_sites};
    use serde_json::{Value, json};
    use sha2::{Digest, Sha256};

    fn c_only() -> Variants {
        Variants { quote: Some(Quote::Curly), dash: Some(Dash::EmClosed), ellipsis: Some(Ellipsis::Char), emphasis: Some("**"), ..Default::default() }
    }

    fn l_only() -> Variants {
        Variants { trailing_whitespace: Some(" ".into()), separator: Some("\n\n".into()), ..Default::default() }
    }

    fn both() -> Variants {
        Variants { trailing_whitespace: Some(" ".into()), separator: Some("\n\n".into()), ..c_only() }
    }

    /// Synthetic human-like text (written for these tests): ASCII quotes and
    /// apostrophes, spaced and double hyphens, three-dot ellipses and
    /// single-newline paragraphs.
    fn human_text() -> String {
        [
            "\"We left early,\" she said - mostly to herself - and the bus was late again.",
            "It wasn't the first time... the driver had a long route through the old town.",
            "Nobody minded much -- the morning was mild and the shops were opening their doors.",
            "By noon the 'quiet' streets were full of people walking home for lunch.",
        ]
        .join("\n")
    }

    /// Alphanumeric runs: the word sequence every injection keeps.
    fn alnum(s: &str) -> Vec<String> {
        s.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).map(str::to_string).collect()
    }

    /// The record names no word of six or more letters from the text.
    fn assert_text_free(rec: &Value, text: &str) {
        let s = serde_json::to_string(rec).unwrap();
        for w in alnum(text).iter().filter(|w| w.chars().count() >= 6) {
            assert!(!s.contains(w.as_str()), "record holds the word {w}");
        }
    }

    fn conv(v: &Variants) -> Conventions {
        let q = Choice { value: v.quote, source: "t" };
        Conventions { double: q, single: q, dash: Choice { value: v.dash, source: "t" }, range: Choice { value: None, source: "t" }, ellipsis: Choice { value: v.ellipsis, source: "t" } }
    }

    #[test]
    fn c_twin_renders_the_variants_and_keeps_the_words() {
        let t = human_text();
        let a = inject(&t, &[], Group::C, &c_only(), "doc-1").unwrap();
        assert_eq!(alnum(&a.text), alnum(&t), "the word sequence is unchanged");
        assert!(!a.text.contains(['"', '\'']), "every quote is curly");
        assert!(a.text.contains("said\u{2014}mostly") && a.text.contains("herself\u{2014}and") && !a.text.contains(" - "));
        assert!(a.text.contains("much\u{2014}the") && !a.text.contains("--"));
        assert!(a.text.contains("time\u{2026} the") && !a.text.contains("..."));
        assert!(a.text.contains("\u{201C}We left early,\u{201D}") && a.text.contains("wasn\u{2019}t") && a.text.contains("\u{2018}quiet\u{2019}"));
        // One emphasis pair, on one line, around two plain words.
        assert_eq!(a.text.matches("**").count(), 2);
        let line = a.text.split('\n').find(|l| l.contains("**")).unwrap();
        assert_eq!(line.matches("**").count(), 2);
        let inner = line.split("**").nth(1).unwrap();
        assert_eq!(inner.split(' ').count(), 2);
        assert!(inner.chars().all(|c| c.is_alphanumeric() || c == ' '));
        // Three spaced sites rendered closed join three pairs of words; layout is untouched.
        assert_eq!(a.record["words_before"].as_u64().unwrap() - a.record["words_after"].as_u64().unwrap(), 6);
        assert_eq!(a.text.matches('\n').count(), t.matches('\n').count());
        assert_eq!(a.record["edits_by_class"]["dash"], 3);
        assert_eq!(a.record["edits_by_class"]["ellipsis"], 1);
        assert_eq!(a.record["edits_by_class"]["emphasis"], 2);
        assert_eq!(a.record["group"], "h_inj_c");
        assert_eq!(a.record["variants"], json!({"quote": "curly", "dash": "em_closed", "ellipsis": "char", "emphasis": "**"}));
        assert_eq!(a.record["text_changed"], true);
        assert_text_free(&a.record, &t);
        // The L variants do nothing in a C twin, and no variant leaves the text as it is.
        assert_eq!(inject(&t, &[], Group::C, &both(), "doc-1").unwrap().text, a.text);
        assert_eq!(inject(&t, &[], Group::C, &Variants::default(), "doc-1").unwrap().text, t);
        assert_eq!(inject(&t, &[], Group::L, &c_only(), "doc-1").unwrap().text, t);
    }

    #[test]
    fn c_rendering_equals_the_form_crate_rendering_line_by_line() {
        let lines = [
            "\"Go,\" he said\u{2014}\"now.\" It's 'tis the season, 'em too.",
            "She paused \u{2013} then left -- quietly -- before the end... or after\u{2026}",
            "Years 1990-1995 and 2001\u{2013}2004 stay ranges; a\u{2014}b and c--d are closed.",
            "A mixed line: x \u{2014} y, p - q and \u{201C}curly\u{201D} with \u{2018}single\u{2019} marks.",
            "Edge: he said '\u{2014}quietly' and \"\u{2014}loudly\" to them.",
            "  Indented line with trailing space ... and tabs\t- here.  ",
            "",
            "Last line without dashes but with 'quotes' and \"more\".\r",
        ];
        let t = lines.join("\n");
        for q in [None, Some(Quote::Curly), Some(Quote::Ascii)] {
            for d in std::iter::once(None).chain(DASHES.iter().copied().map(Some)) {
                for e in std::iter::once(None).chain(ELLIPSES.iter().copied().map(Some)) {
                    let v = Variants { quote: q, dash: d, ellipsis: e, ..Default::default() };
                    let a = inject(&t, &[], Group::C, &v, "r").unwrap();
                    let c = conv(&v);
                    let want: Vec<String> = t.split('\n').map(|l| render_quotes(&render_sites(l, &c, &mut Changes::default()), &c, &mut Changes::default())).collect();
                    assert_eq!(a.text, want.join("\n"), "{q:?} {d:?} {e:?}");
                    assert_eq!(alnum(&a.text), alnum(&t));
                }
            }
        }
    }

    #[test]
    fn overlapping_sites_are_skipped_not_fatal() {
        // Two spaced hyphens one space apart are overlapping sites; render_sites
        // cannot replace both, so the second is skipped and counted.
        let t = "It was - - odd, they all agreed at the end of the long day.";
        let a = inject(t, &[], Group::C, &Variants { dash: Some(Dash::EmClosed), ..Default::default() }, "o").unwrap();
        assert_eq!(a.text, "It was\u{2014}- odd, they all agreed at the end of the long day.");
        assert_eq!(a.record["overlapping_sites_skipped"], 1);
        assert_eq!(alnum(&a.text), alnum(t));
        let b = inject(t, &[], Group::C, &Variants { dash: Some(Dash::EmSpaced), ..Default::default() }, "o").unwrap();
        assert_eq!(b.text, "It was \u{2014} - odd, they all agreed at the end of the long day.");
    }

    #[test]
    fn emphasis_takes_the_keyed_phrase_or_none() {
        let t = human_text();
        let a = inject(&t, &[], Group::C, &Variants { emphasis: Some("__"), ..Default::default() }, "doc-7").unwrap();
        // The independent rule: candidates are two consecutive all-alphanumeric
        // words on one line; the smallest V21 key of the first word's index wins.
        let ws: Vec<(usize, &str)> = {
            let mut v = Vec::new();
            let mut off = 0;
            for w in t.split_whitespace() {
                let s = off + t[off..].find(w).unwrap();
                v.push((s, w));
                off = s + w.len();
            }
            v
        };
        let plain = |w: &str| w.chars().all(char::is_alphanumeric);
        let mut cands: Vec<(String, usize)> = (0..ws.len() - 1)
            .filter(|&k| plain(ws[k].1) && plain(ws[k + 1].1) && !t[ws[k].0 + ws[k].1.len()..ws[k + 1].0].contains('\n'))
            .map(|k| (hex::encode(Sha256::digest(format!("slop_ninja_a_comp_r2_plan_v1:h_inj_c_phrase:doc-7:{k}").as_bytes())), k))
            .collect();
        cands.sort();
        let k = cands[0].1;
        assert_eq!(a.record["emphasis"]["word_index"], k);
        assert_eq!(a.record["emphasis"]["candidates"], cands.len());
        assert!(a.text.contains(&format!("__{} {}__", ws[k].1, ws[k + 1].1)));
        assert_eq!(a.text.replace("__", ""), t);
        // Deterministic per row key, and keyed by it.
        assert_eq!(inject(&t, &[], Group::C, &Variants { emphasis: Some("__"), ..Default::default() }, "doc-7").unwrap().text, a.text);
        let others: std::collections::BTreeSet<String> =
            (0..8).map(|i| inject(&t, &[], Group::C, &Variants { emphasis: Some("__"), ..Default::default() }, &format!("k{i}")).unwrap().text).collect();
        assert!(others.len() > 1);
        // No two plain words on one line: nothing is placed.
        let none = "Wait... what?! No\u{2014}never, ever.\nHe said: \"fine.\"";
        let b = inject(none, &[], Group::C, &Variants { emphasis: Some("**"), ..Default::default() }, "x").unwrap();
        assert_eq!(b.text, none);
        assert_eq!(b.record["emphasis"], json!({"placed": false, "candidates": 0, "word_index": null}));
        // A pair split by a line break is not a phrase.
        let split = "alpha\nbeta";
        assert_eq!(inject(split, &[], Group::C, &Variants { emphasis: Some("**"), ..Default::default() }, "x").unwrap().text, split);
    }

    #[test]
    fn l_twin_trailing_whitespace_on_the_keyed_half() {
        let lines: Vec<String> = (0..7).map(|i| format!("Line number {i} of the synthetic note ends here.")).collect();
        let t = lines.join("\n");
        let v = Variants { trailing_whitespace: Some("  ".into()), ..Default::default() };
        let a = inject(&t, &[], Group::L, &v, "doc-3").unwrap();
        let got: Vec<usize> = a.text.split('\n').enumerate().filter(|(_, l)| l.ends_with("  ")).map(|(i, _)| i).collect();
        // The independent rule: ceil(7 / 2) = 4 lines, the smallest V21 keys.
        let mut ranked: Vec<(String, usize)> =
            (0..7).map(|i| (hex::encode(Sha256::digest(format!("slop_ninja_a_comp_r2_plan_v1:h_inj_l_line:doc-3:{i}").as_bytes())), i)).collect();
        ranked.sort();
        let mut want: Vec<usize> = ranked[..4].iter().map(|x| x.1).collect();
        want.sort();
        assert_eq!(got, want);
        assert_eq!(keyed_half(7, "doc-3"), want);
        assert_eq!(keyed_half(8, "doc-3").len(), 4);
        assert_eq!(keyed_half(1, "doc-3"), vec![0]);
        assert!(keyed_half(0, "doc-3").is_empty());
        // Deterministic, keyed by the row key, and whitespace only.
        assert_eq!(inject(&t, &[], Group::L, &v, "doc-3").unwrap().text, a.text);
        let sets: std::collections::BTreeSet<Vec<usize>> = (0..8).map(|i| keyed_half(7, &format!("doc-{i}"))).collect();
        assert!(sets.len() > 1);
        assert!(a.text.split_whitespace().eq(t.split_whitespace()));
        assert_eq!(a.record["layout"]["lines_selected"], 4);
        assert_eq!(a.record["edits_by_class"]["trailing_whitespace"], 4);
        assert_eq!(plan_key(LINE_KEY, "doc-3:0"), ranked.iter().find(|x| x.1 == 0).unwrap().0);
        // The variant goes before a CRLF line's '\r', so CRLF endings stay CRLF.
        let crlf = "First line of the note.\r\nSecond line of the note.\r\nThird line.";
        let b = inject(crlf, &[], Group::L, &Variants { trailing_whitespace: Some(" ".into()), ..Default::default() }, "c").unwrap();
        assert_eq!(b.text.matches("\r\n").count(), 2);
        assert_eq!(b.text.matches(" \r\n").count() + usize::from(b.text.ends_with("line. ")), 2);
    }

    #[test]
    fn l_twin_separators_replace_boundaries_and_keep_wraps() {
        // Hard-wrapped: wraps stay, the sentence break becomes the separator.
        let t = "The quick brown fox jumps over\nthe lazy dog. It ran.\nThen it slept under the old\noak tree for an hour.";
        let v = Variants { separator: Some("\n\n".into()), ..Default::default() };
        let a = inject(t, &[], Group::L, &v, "w").unwrap();
        assert_eq!(a.text, "The quick brown fox jumps over\nthe lazy dog. It ran.\n\nThen it slept under the old\noak tree for an hour.");
        assert_eq!(a.record["layout"]["boundaries"], 1);
        assert_eq!(a.record["layout"]["separators_replaced"], 1);
        // Existing separators of other forms become the variant; leading
        // whitespace, the head and the tail stay.
        let u = "\nFirst paragraph is here and done.\n \n  Second one is indented now.\n\n\nThird is plain.\n";
        let b = inject(u, &[], Group::L, &v, "w").unwrap();
        assert_eq!(b.text, "\nFirst paragraph is here and done.\n\n  Second one is indented now.\n\nThird is plain.\n");
        assert!(b.text.split_whitespace().eq(u.split_whitespace()));
        // Already in the variant: unchanged.
        let same = "One paragraph ends here.\n\nAnother one ends here.";
        assert_eq!(inject(same, &[], Group::L, &v, "w").unwrap().text, same);
        // Both L classes together.
        let c = inject("Alpha line ends.\nBeta line ends.", &[], Group::L, &l_only(), "k").unwrap();
        assert!(c.text == "Alpha line ends. \n\nBeta line ends." || c.text == "Alpha line ends.\n\nBeta line ends. ", "{:?}", c.text);
        assert_text_free(&c.record, "Alpha line ends.\nBeta line ends.");
    }

    #[test]
    fn spans_are_remapped_exactly() {
        let t = human_text();
        // A model span over the whole second line.
        let s = t.find("It wasn't").unwrap();
        let e = t.find("old town.").unwrap() + "old town.".len();
        for g in GROUPS {
            for v in [c_only(), l_only(), both()] {
                for key in ["a", "b", "c", "d"] {
                    let a = inject(&t, &[(s, e)], g, &v, key).unwrap();
                    assert_eq!(a.spans.len(), 1);
                    let (ns, ne) = a.spans[0];
                    // The span still holds exactly the second line's words, rendered.
                    let inner = &a.text[ns..ne];
                    assert_eq!(alnum(inner), alnum(&t[s..e]), "{g:?} {key}");
                    assert_eq!(alnum(&a.text[..ns]), alnum(&t[..s]));
                    assert_eq!(alnum(&a.text[ne..]), alnum(&t[e..]));
                    let labels = word_labels(&a.text, &a.spans).unwrap();
                    assert_eq!(labels.iter().filter(|w| w.1 == 1).count(), inner.split_whitespace().count());
                    // The same text labelled human gets the same bytes (no guard fired).
                    assert_eq!(a.text, inject(&t, &[], g, &v, key).unwrap().text);
                }
            }
        }
        // L: a span edge inside a separator moves to its start; the span keeps its words.
        let u = "First sentence is here.\n \nSecond sentence is here.\nThird one.";
        let gap = u.find('\n').unwrap();
        let end = u.find("here.\nThird").unwrap() + 5;
        let a = inject(u, &[(gap + 1, end)], Group::L, &Variants { separator: Some("\n\n".into()), ..Default::default() }, "s").unwrap();
        assert_eq!(a.text, "First sentence is here.\n\nSecond sentence is here.\n\nThird one.");
        assert_eq!(a.spans, vec![(gap, gap + 2 + "Second sentence is here.".len())]);
    }

    #[test]
    fn joining_dashes_never_take_a_span_edge() {
        let t = "We walked home - and then we ran the rest of the way back.";
        let at = t.find("and").unwrap();
        let v = Variants { dash: Some(Dash::EmClosed), ..Default::default() };
        // Mixed: the span starts at the word after the dash, so the join is skipped.
        let m = inject(t, &[(at, t.len())], Group::C, &v, "g").unwrap();
        assert_eq!(m.text, t);
        assert_eq!(m.record["span_guard_skips"], json!({"dash": 1}));
        // Human and model-only labellings join it, byte for byte alike.
        let h = inject(t, &[], Group::C, &v, "g").unwrap();
        let mo = inject(t, &[(0, t.len())], Group::C, &v, "g").unwrap();
        assert_eq!(h.text, "We walked home\u{2014}and then we ran the rest of the way back.");
        assert_eq!(mo.text, h.text);
        assert_eq!(mo.spans, vec![(0, mo.text.len())]);
        assert_eq!(h.record["span_guard_skips"], json!({}));
        // A span starting at the dash itself: a glyph-only change keeps it
        // exact; a join is skipped.
        let d = t.find('-').unwrap();
        let sp = inject(t, &[(d, t.len())], Group::C, &Variants { dash: Some(Dash::EmSpaced), ..Default::default() }, "g").unwrap();
        assert_eq!(sp.text, "We walked home \u{2014} and then we ran the rest of the way back.");
        assert_eq!(sp.spans, vec![(d, sp.text.len())]);
        assert_eq!(&sp.text[d..d + 3], "\u{2014}");
        let jn = inject(t, &[(d, t.len())], Group::C, &v, "g").unwrap();
        assert_eq!(jn.text, t);
        // Closed dashes rendered spaced split a word inside one span: always exact.
        let c = "Both of them\u{2014}tired and cold\u{2014}walked on toward the town.";
        let s0 = c.find("them").unwrap();
        let cs = inject(c, &[(s0, c.len())], Group::C, &Variants { dash: Some(Dash::HyphenSpaced), ..Default::default() }, "g").unwrap();
        assert_eq!(cs.text, "Both of them - tired and cold - walked on toward the town.");
        assert_eq!(cs.spans, vec![(s0, cs.text.len())]);
        assert_eq!(word_labels(&cs.text, &cs.spans).unwrap().iter().filter(|w| w.1 == 0).count(), 2);
    }

    #[test]
    fn emphasis_markers_follow_their_words_across_span_edges() {
        // The span is exactly the middle line "alpha beta"; find a row key whose
        // phrase is that pair, so the opening marker sits on the span's start
        // and the closing marker on its end.
        let t = "First words here.\nalpha beta\nLast words here.";
        let (s, e) = (t.find("alpha").unwrap(), t.find("beta").unwrap() + 4);
        let v = Variants { emphasis: Some("**"), ..Default::default() };
        let key = (0..200).map(|i| format!("k{i}")).find(|k| inject(t, &[], Group::C, &v, k).unwrap().record["emphasis"]["word_index"] == 3).unwrap();
        let a = inject(t, &[(s, e)], Group::C, &v, &key).unwrap();
        assert_eq!(a.text, "First words here.\n**alpha beta**\nLast words here.");
        assert_eq!(&a.text[a.spans[0].0..a.spans[0].1], "**alpha beta**", "both markers belong to the span's words");
        // The span on the other lines: the markers stay outside it.
        let (s2, e2) = (t.find("Last").unwrap(), t.len());
        let b = inject(t, &[(0, t.find("here.").unwrap() + 5), (s2, e2)], Group::C, &v, &key).unwrap();
        assert_eq!(b.text, a.text);
        assert_eq!(&b.text[b.spans[0].0..b.spans[0].1], "First words here.");
        assert_eq!(&b.text[b.spans[1].0..b.spans[1].1], "Last words here.");
        // A span ending where the pair starts, and one starting where it ends.
        let c = inject(t, &[(0, s), (e, t.len())], Group::C, &v, &key).unwrap();
        assert_eq!(&c.text[..c.spans[0].1], "First words here.\n");
        assert_eq!(&c.text[c.spans[1].0..], "\nLast words here.");
    }

    #[test]
    fn injection_is_label_blind() {
        let texts = [human_text(), "Short one - with a dash... and \"quotes\".\nSecond line here.\n\nThird after a blank.".to_string()];
        for t in &texts {
            for g in GROUPS {
                for key in ["k1", "k2", "k3"] {
                    let h = inject(t, &[], g, &both(), key).unwrap();
                    let m = inject(t, &[(0, t.len())], g, &both(), key).unwrap();
                    assert_eq!(h.text, m.text, "{g:?} {key}");
                    assert_eq!(m.spans, vec![(0, m.text.len())]);
                    assert!(h.spans.is_empty());
                    assert_eq!(h.record["edits_by_class"], m.record["edits_by_class"]);
                }
            }
        }
    }

    #[test]
    fn audit_counts_and_chooses_per_class() {
        let humans = [
            "\"Plain,\" he said - flatly.\nNext line of the note is here.",
            "It's late... we'll go.\nAnother line ends here.\r\nThird.",
            "No marks at all in this short human note today.",
        ];
        let positives = [
            "\u{201C}Curly,\u{201D} she said\u{2014}softly. It\u{2019}s **really true**. \n\nWait\u{2026} \n\nDone here.",
            "The plan\u{2014}simple\u{2014}worked. A __small__ __note__ here, it\u{2019}s done.\n\nEnd of it.",
        ];
        let mut au = InjectAudit::default();
        for h in humans {
            au.add(false, h);
        }
        for p in positives {
            au.add(true, p);
        }
        let rep = au.report().unwrap();
        assert_eq!((rep["positives"].clone(), rep["humans"].clone()), (json!(2), json!(3)));
        let q = &rep["classes"]["quote"];
        // curly: 2/2 vs 0/3; ascii: 0/2 vs 2/3.
        assert_eq!(q["variants"][0], json!({"variant": "curly", "positives_showing": 2, "humans_showing": 0, "positive_share": 1.0, "human_share": 0.0, "difference": 1.0, "score_numerator": "6"}));
        assert_eq!(q["variants"][1]["variant"], "ascii");
        assert_eq!(q["variants"][1]["score_numerator"], "-4");
        assert_eq!(q["chosen"], "curly");
        let v = &rep["variants"];
        assert_eq!(v["dash"], "em_closed");
        assert_eq!(v["ellipsis"], "char");
        // "**" in one positive, "__" in the other: a tie broken by the fixed order.
        assert_eq!(v["emphasis"], "**");
        assert_eq!(v["trailing_whitespace"], "\\s");
        assert_eq!(v["separator"], "\\n\\n");
        // A CRLF ending is not trailing whitespace.
        assert!(shown(humans[1])[&Class::TrailingWhitespace].is_empty());
        // Single breaks are not separator variants (only blank-line runs are).
        assert!(shown(humans[0])[&Class::Separator].is_empty());
        // Variants more common in humans, or equally common (the ellipsis
        // character, score 0), are never chosen: no variant at all.
        let mut au2 = InjectAudit::default();
        au2.add(true, "A plain positive line\u{2026} with nothing special in it.");
        au2.add(false, "A human \u{201C}quote\u{201D} here - and there\u{2026} now.");
        let v2 = au2.variants().unwrap();
        assert_eq!((v2.quote, v2.dash, v2.ellipsis, v2.emphasis), (None, None, None, None));
        assert_eq!(au2.report().unwrap()["variants"]["quote"], Value::Null);
        // Equal score, larger positive count wins (over the fixed order).
        let mut au3 = InjectAudit::default();
        for t in ["a\u{2014}b and x \u{2014} y.", "x \u{2014} y z."] {
            au3.add(true, t);
        }
        for t in ["x \u{2014} y z.", "x \u{2014} y w.", "q r s.", "t u v."] {
            au3.add(false, t);
        }
        // em_closed: 1*4 - 0*2 = 4; em_spaced: 2*4 - 2*2 = 4, with two positives.
        assert_eq!(au3.variants().unwrap().dash, Some(Dash::EmSpaced));
        let mut au4 = InjectAudit::default();
        for t in ["x \u{2014} y z.", "a\u{2014}b c.", "a\u{2014}b c and x \u{2014} y."] {
            au4.add(true, t);
        }
        au4.add(false, "a\u{2014}b c.");
        au4.add(false, "x \u{2014} y z.");
        // Both 2*2 - 1*3 = 1 with two positives each: the fixed order (em_closed first).
        assert_eq!(au4.variants().unwrap().dash, Some(Dash::EmClosed));
        // The audit refuses an empty side.
        let mut au5 = InjectAudit::default();
        au5.add(true, "only positives here.");
        assert!(au5.report().is_err());
        assert_text_free(&rep, &(humans.join(" ") + &positives.join(" ")));
    }

    #[test]
    fn variants_json_round_trips_and_validates() {
        let v = both();
        let j = v.to_json();
        assert_eq!(j, json!({"quote": "curly", "dash": "em_closed", "ellipsis": "char", "emphasis": "**", "trailing_whitespace": "\\s", "separator": "\\n\\n"}));
        assert_eq!(Variants::from_json(&j).unwrap(), v);
        let empty = Variants::default().to_json();
        assert_eq!(Variants::from_json(&empty).unwrap(), Variants::default());
        for d in DASHES {
            let mut x = Variants::default();
            x.set(Class::Dash, Some(dash_name(d))).unwrap();
            assert_eq!(x.dash, Some(d));
        }
        let bad = |k: &str, val: Value| {
            let mut o = j.clone();
            o[k] = val;
            Variants::from_json(&o)
        };
        assert!(bad("dash", json!("em")).is_err());
        assert!(bad("quote", json!("smart")).is_err());
        assert!(bad("emphasis", json!("*")).is_err());
        assert!(bad("separator", json!("\\s")).is_err(), "a separator starts and ends with a newline");
        assert!(bad("separator", json!("\\n")).is_err(), "a separator is a blank-line run");
        assert!(bad("separator", json!("\\n\\r\\n")).is_err(), "separator variants are in LF form");
        assert!(bad("separator", json!("\\n\\s\\n")).is_ok() && bad("separator", json!("\\n\\n\\n")).is_ok());
        assert!(bad("trailing_whitespace", json!("\\s\\n")).is_err());
        assert!(bad("trailing_whitespace", json!("\\s\\r")).is_err());
        assert!(bad("trailing_whitespace", json!("\\u{0020}")).is_err(), "not the canonical escape");
        assert!(bad("trailing_whitespace", json!("x")).is_err());
        assert!(bad("trailing_whitespace", json!(3)).is_err());
        let mut extra = j.clone();
        extra["range"] = json!(null);
        assert!(Variants::from_json(&extra).is_err());
        let mut missing = j.clone();
        missing.as_object_mut().unwrap().remove("separator");
        assert!(Variants::from_json(&missing).is_err());
        assert_eq!(unescape_ws("\\s\\t\\u{00a0}\\n").unwrap(), " \t\u{a0}\n");
        assert!(unescape_ws("").is_err());
        assert!(unescape_ws("\\u{0041}").is_err());
        assert!(unescape_ws("\\q").is_err());
        assert!(injects(&v, Group::C) && injects(&v, Group::L));
        assert!(!injects(&c_only(), Group::L) && !injects(&l_only(), Group::C));
    }

    #[test]
    fn twins_for_human_rows_only() {
        let t = human_text();
        let h = row("h1", &t, "human_only", &[]);
        let tw = twins_for_row(&h, &both(), "abc").unwrap();
        assert_eq!(tw.len(), 2);
        for ((m, r), kind) in tw.iter().zip(["h_inj_c", "h_inj_l"]) {
            assert_eq!(m["schema"], PROBE_SCHEMA);
            assert_eq!(m["kind"], kind);
            assert_eq!(m["probe_id"], format!("probe:{kind}:h1"));
            assert_eq!(m["base_id"], "h1");
            assert_eq!(m["split"], "train");
            assert_eq!(m["origin"], "human_only");
            assert_eq!(m["values_sha256"], "abc");
            assert_eq!(m["text_changed"], true);
            assert!(m.get("text").is_none());
            assert_text_free(m, &t);
            assert_eq!(r["id"], m["probe_id"]);
            assert_eq!(r["probe_of"], "h1");
            assert_eq!(r["form_inject"]["schema"], RECORD_SCHEMA);
            assert_eq!(r["text_sha256"], m["text_sha256"]);
            for k in ["split", "source_group", "writer_key", "domain", "population", "origin"] {
                assert_eq!(r[k], h[k]);
            }
            let (_, origin, spans) = row_parts(r).unwrap();
            assert_eq!((origin, spans.len()), ("human_only", 0));
            assert_eq!(r["share"], 0.0);
            assert_eq!(r["word_count"], r["text"].as_str().unwrap().split_whitespace().count());
            assert_eq!(alnum(r["text"].as_str().unwrap()), alnum(&t));
        }
        assert_eq!(tw[0].0["variants"], json!({"quote": "curly", "dash": "em_closed", "ellipsis": "char", "emphasis": "**"}));
        assert_eq!(tw[1].0["variants"], json!({"trailing_whitespace": "\\s", "separator": "\\n\\n"}));
        // Mixed and model-only rows get no twin.
        let s = t.find("It wasn't").unwrap();
        assert!(twins_for_row(&row("m1", &t, "mixed", &[(s, t.len())]), &both(), "abc").unwrap().is_empty());
        assert!(twins_for_row(&row("o1", &t, "model_only", &[(0, t.len())]), &both(), "abc").unwrap().is_empty());
    }

    // ------------------------------------------------------------ review fixes

    use grammar_a_form::measure::{Measure, escape_ws};
    use std::path::{Path, PathBuf};

    /// Blank-line runs of a text (gaps of two or more newlines).
    fn blank_runs(t: &str) -> usize {
        Measure::of(t).gaps.iter().filter(|g| g.newlines >= 2).count()
    }

    /// A synthetic excerpt-layout note (paragraphs joined by one empty line),
    /// optionally with a two-item list paragraph.
    fn excerpt(i: usize, list: bool) -> String {
        let mut p = vec![
            format!("Paragraph one of note {i} talks about the weather and the long walk home."),
            format!("Paragraph two of note {i} ends the story with a quiet supper at the table."),
        ];
        if list {
            p.insert(1, "Things to buy:\n- bread\n- milk".to_string());
        }
        p.join("\n\n")
    }

    fn set1(c: Class, name: &str) -> Variants {
        let mut v = Variants::default();
        v.set(c, Some(name)).unwrap();
        v
    }

    fn sorted(set: &std::collections::BTreeSet<String>) -> Vec<String> {
        set.iter().cloned().collect()
    }

    #[test]
    fn separator_variants_are_blank_line_runs_only() {
        // Review finding 1: excerpt-layout humans (three with a two-item list)
        // against positives that all have blank lines and a list. Line breaks
        // never make a single newline a separator variant.
        let mut au = InjectAudit::default();
        for i in 0..13 {
            au.add(false, &excerpt(i, i < 3));
        }
        for i in 0..10 {
            au.add(true, &format!("Answer {i} starts with a short overview of the plan:\n- first point\n- second point\n\nThe closing paragraph of answer {i} sums it up."));
        }
        let rep = au.report().unwrap();
        let rows = rep["classes"]["separator"]["variants"].as_array().unwrap().clone();
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(rows[0]["variant"], "\\n\\n");
        assert_eq!((rows[0]["positives_showing"].clone(), rows[0]["humans_showing"].clone(), rows[0]["score_numerator"].clone()), (json!(10), json!(13), json!("0")));
        assert_eq!(rep["variants"]["separator"], Value::Null);
        // An over-represented blank-line run wins; single newlines never compete.
        let mut au2 = InjectAudit::default();
        for i in 0..4 {
            au2.add(false, &format!("Line {i} of a human note.\nAnother line of it.\nA third line."));
        }
        au2.add(false, &excerpt(9, false));
        for i in 0..4 {
            au2.add(true, &format!("{}\n- a\n- b\n- c", excerpt(i, false)));
        }
        assert_eq!(au2.variants().unwrap().separator.as_deref(), Some("\n\n"));
    }

    #[test]
    fn single_breaks_show_no_separator() {
        // Hard wraps (an Enron-like hard-wrapped human), sentence breaks and
        // line breaks are not separator variants; blank-line runs are, in LF form.
        let wrapped = "A hard wrapped paragraph of the synthetic note goes on\nfor a while and then it ends here. Then another short\nsentence follows and stops.";
        assert!(Measure::of(wrapped).hard_wrapped);
        assert!(shown(wrapped)[&Class::Separator].is_empty(), "wraps are not separators");
        let lines = "One sentence ends here.\nAnother sentence ends here.\nTitle line\nBody text of the note goes on.";
        assert!(shown(lines)[&Class::Separator].is_empty(), "sentence and line breaks are not separators");
        assert_eq!(sorted(&shown(&format!("{wrapped}\n\n{lines}"))[&Class::Separator]), vec!["\\n\\n".to_string()]);
        assert_eq!(sorted(&shown("One line ends.\r\n\r\nNext line ends.")[&Class::Separator]), vec!["\\n\\n".to_string()]);
        assert_eq!(sorted(&shown("One line ends.\r\n \r\n\r\nNext.")[&Class::Separator]), vec!["\\n\\s\\n\\n".to_string()]);
        // A hard-wrapped human against unwrapped positives: no single-newline variant.
        let mut au = InjectAudit::default();
        au.add(false, wrapped);
        au.add(false, &excerpt(1, false));
        au.add(true, &excerpt(2, false));
        let rep = au.report().unwrap();
        assert!(rep["classes"]["separator"]["variants"].as_array().unwrap().iter().all(|r| r["variant"] != "\\n"));
    }

    #[test]
    fn l_twin_never_removes_a_blank_line() {
        let texts = [
            excerpt(1, false),
            excerpt(2, true),
            "P1.\n\nP2.".to_string(),
            "First paragraph of the note ends here.\nSecond paragraph follows on the next line.\n\n\nThird after two blank lines.".to_string(),
            "<amy> hi all\n<bob> hey amy\n<amy> how goes\n\n<bob> fine".to_string(),
            "A hard wrapped paragraph of the synthetic note goes on\nfor a while and then it ends here. Then another short\nsentence follows and stops.\n\nNext paragraph.".to_string(),
        ];
        for sep in ["\\n\\n", "\\n\\n\\n", "\\n\\s\\n"] {
            let v = set1(Class::Separator, sep);
            for t in &texts {
                let a = inject(t, &[], Group::L, &v, "b").unwrap();
                assert!(blank_runs(&a.text) >= blank_runs(t), "{sep} {t:?} -> {:?}", a.text);
                let (m0, m1) = (Measure::of(t), Measure::of(&a.text));
                assert_eq!(m1.lines.len(), m0.lines.len());
                for (i, g) in m0.gaps.iter().enumerate() {
                    if g.boundary() {
                        assert_eq!(escape_ws(m1.separator(i)), sep, "{t:?}");
                    } else {
                        assert_eq!(m1.separator(i), "\n", "wraps stay");
                    }
                }
            }
        }
        assert_eq!(inject("P1.\n\nP2.", &[], Group::L, &l_only(), "b").unwrap().text.matches("\n\n").count(), 1);
    }

    #[test]
    fn crlf_separators_stay_crlf() {
        let v = set1(Class::Separator, "\\n\\n");
        let same = "One line ends.\r\n\r\nNext line ends.";
        assert_eq!(inject(same, &[], Group::L, &v, "c").unwrap().text, same);
        let single = "First sentence ends here.\r\nSecond sentence ends here.";
        let a = inject(single, &[], Group::L, &v, "c").unwrap();
        assert_eq!(a.text, "First sentence ends here.\r\n\r\nSecond sentence ends here.");
        assert_eq!(a.record["layout"]["boundaries_after_crlf_lines"], 1);
        let v3 = set1(Class::Separator, "\\n\\s\\n\\n");
        assert_eq!(inject(single, &[], Group::L, &v3, "c").unwrap().text, "First sentence ends here.\r\n \r\n\r\nSecond sentence ends here.");
        let mixed = "Alpha line ends.\nBeta line ends.\r\nGamma line ends.";
        assert_eq!(inject(mixed, &[], Group::L, &v, "c").unwrap().text, "Alpha line ends.\n\nBeta line ends.\r\n\r\nGamma line ends.");
        assert_eq!(lf("a\r\n\r\r\nb\rc\r"), "a\n\nb\rc\r");
        assert_eq!(lf(&lf("\n\r\r\n")), lf("\n\r\r\n"));
    }

    /// A line's trailing whitespace, less its final '\r' run.
    fn trail_of(l: &str) -> &str {
        let l = l.trim_end_matches('\r');
        &l[l.trim_end().len()..]
    }

    #[test]
    fn trailing_whitespace_is_set_not_appended() {
        let t = "First line of the note ends here.  \nSecond line of the note ends here.\t\t\nThird line ends here. \r\nFourth line ends here.";
        let v = set1(Class::TrailingWhitespace, "\\s");
        let sel = keyed_half(4, "tw");
        let a = inject(t, &[], Group::L, &v, "tw").unwrap();
        let (got, orig): (Vec<&str>, Vec<&str>) = (a.text.split('\n').collect(), t.split('\n').collect());
        for i in 0..4 {
            if sel.contains(&i) {
                assert_eq!(trail_of(got[i]), " ", "line {i}: the variant is the line's trailing whitespace");
            } else {
                assert_eq!(got[i], orig[i]);
            }
            assert_eq!(got[i].ends_with('\r'), orig[i].ends_with('\r'));
        }
        assert_eq!(a.record["layout"]["selected_lines_with_trailing_whitespace"], sel.iter().filter(|&&i| i < 3).count());
        assert!(shown(&a.text)[&Class::TrailingWhitespace].contains("\\s"));
        // Already in the variant: no edit.
        let same = "Line one ends. \nLine two ends. ";
        let b = inject(same, &[], Group::L, &v, "x").unwrap();
        assert_eq!(b.text, same);
        assert_eq!(b.record["layout"]["selected_lines_already_in_variant"], 1);
        assert_eq!(b.record["text_changed"], false);
    }

    #[test]
    fn lone_markers_are_not_emphasis() {
        assert!(shown("A lone ** marker sits here.")[&Class::Emphasis].is_empty());
        assert!(shown("A lone __ marker\nand another __ on the next line.")[&Class::Emphasis].is_empty());
        assert_eq!(sorted(&shown("Some **bold** words and __more__ here.")[&Class::Emphasis]), vec!["**".to_string(), "__".to_string()]);
    }

    #[test]
    fn touching_sites_are_both_rendered() {
        let t = "We waited...--then the bus came around the corner at last.";
        let v = Variants { dash: Some(Dash::EmClosed), ellipsis: Some(Ellipsis::Char), ..Default::default() };
        let a = inject(t, &[], Group::C, &v, "t").unwrap();
        assert_eq!(a.text, "We waited\u{2026}\u{2014}then the bus came around the corner at last.");
        assert_eq!(a.record["overlapping_sites_skipped"], 0);
        let c = conv(&v);
        assert_eq!(a.text, render_quotes(&render_sites(t, &c, &mut Changes::default()), &c, &mut Changes::default()));
    }

    #[test]
    fn whitespace_ties_go_to_byte_order() {
        // "\s" and "\t" each in one positive, in no human: equal score and
        // positive count; the escaped strings' byte order puts "\s" first.
        let mut au = InjectAudit::default();
        au.add(true, "First positive line ends here. \nSecond line.");
        au.add(true, "Another positive line ends.\t\nMore text.");
        au.add(false, "A human line.\nAnother human line.");
        assert_eq!(au.variants().unwrap().trailing_whitespace.as_deref(), Some(" "));
        // "\n\n" before "\n\s\n".
        let mut au2 = InjectAudit::default();
        au2.add(true, "One paragraph.\n \nTwo paragraph.");
        au2.add(true, "One paragraph.\n\nTwo paragraph.");
        au2.add(false, "One human paragraph only.");
        assert_eq!(au2.variants().unwrap().separator.as_deref(), Some("\n\n"));
    }

    #[test]
    fn variants_come_from_the_frozen_register() {
        let good = json!({"schema": VALUES_SCHEMA, "values": {"S0-7": {"x": 1}, "S0-13": {"variants": both().to_json(), "audit": {"sha256": "ab"}}}});
        let (v, e) = variants_from_values(&good).unwrap();
        assert_eq!(v, both());
        assert_eq!(e["audit"]["sha256"], "ab");
        assert!(variants_from_values(&json!({"schema": VALUES_SCHEMA, "values": {"S0-7": {}}})).is_err(), "no S0-13 entry");
        assert!(variants_from_values(&json!({"schema": "other", "values": {"S0-13": {"variants": both().to_json()}}})).is_err());
        assert!(variants_from_values(&json!({"schema": VALUES_SCHEMA, "values": {"S0-13": {"audit": {}}}})).is_err());
        let mut bad = good.clone();
        bad["values"]["S0-13"]["variants"]["separator"] = json!("\\n");
        assert!(variants_from_values(&bad).is_err());
        // An audit record is not the register.
        assert!(variants_from_values(&json!({"schema": AUDIT_SCHEMA, "variants": both().to_json()})).is_err());
    }

    /// A scratch directory removed on drop.
    struct TmpDir(PathBuf);

    impl TmpDir {
        fn new(name: &str) -> TmpDir {
            let p = std::env::temp_dir().join(format!("grammar-a-augment-test-{}-{name}", std::process::id()));
            let _ = std::fs::remove_dir_all(&p);
            std::fs::create_dir_all(&p).unwrap();
            TmpDir(p)
        }
    }

    impl Drop for TmpDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// An assembler-style Train split directory holding `rows` (manifest
    /// edited by `tweak`).
    fn split_dir(root: &Path, name: &str, rows: &[Value], tweak: impl FnOnce(&mut Value)) -> PathBuf {
        let d = root.join(name);
        std::fs::create_dir_all(&d).unwrap();
        let body: String = rows.iter().map(|r| serde_json::to_string(r).unwrap() + "\n").collect();
        std::fs::write(d.join("rows.jsonl"), &body).unwrap();
        let mut m = json!({"schema": TRAIN_MANIFEST_SCHEMA, "build_id": "b-test", "split": "train", "sealed": false,
            "row_schema": "slop_ninja_a_bundle_row_v1", "rows": rows.len(), "files": {"rows.jsonl": hex::encode(Sha256::digest(body.as_bytes()))}});
        tweak(&mut m);
        std::fs::write(d.join("MANIFEST.json"), serde_json::to_vec(&m).unwrap()).unwrap();
        d
    }

    fn weighted(id: &str, text: &str, origin: &str, spans: &[(usize, usize)], w: f64) -> Value {
        let mut r = row(id, text, origin, spans);
        r["weight"] = json!(w);
        r
    }

    fn span_of(t: &str, part: &str) -> Vec<(usize, usize)> {
        let s = t.find(part).unwrap();
        vec![(s, s + part.len())]
    }

    #[test]
    fn audit_reads_positive_weight_rows_by_role() {
        let tmp = TmpDir::new("roles");
        let mx = "Human words come first here. \u{201C}Then model words,\u{201D} it said.";
        let mpart = "\u{201C}Then model words,\u{201D} it said.";
        let humans = split_dir(&tmp.0, "humans", &[
            weighted("h1", "\"Plain,\" he said to us.", "human_only", &[], 1.0),
            weighted("h2", "Nothing at all here today.", "human_only", &[], 0.5),
            weighted("e1", "\u{201C}Zero weight,\u{201D} it said.", "human_only", &[], 0.0),
            weighted("m1", mx, "mixed", &span_of(mx, mpart), 1.0),
            weighted("o1", "\u{201C}All model text.\u{201D}", "model_only", &[(0, "\u{201C}All model text.\u{201D}".len())], 1.0),
        ], |_| {});
        let p2 = "\u{201C}Curly model text,\u{201D} it said.";
        let p3 = "\"Ascii zero weight.\"";
        let positives = split_dir(&tmp.0, "positives", &[
            weighted("p1", mx, "mixed", &span_of(mx, mpart), 1.0),
            weighted("p2", p2, "model_only", &[(0, p2.len())], 2.0),
            weighted("p3", p3, "model_only", &[(0, p3.len())], 0.0),
            weighted("p4", "\"Human in the positives split.\"", "human_only", &[], 1.0),
        ], |_| {});
        let rep = audit_train_splits(std::slice::from_ref(&humans), std::slice::from_ref(&positives)).unwrap();
        assert_eq!((rep["humans"].clone(), rep["positives"].clone()), (json!(2), json!(2)));
        let q = &rep["classes"]["quote"]["variants"];
        assert_eq!((q[0]["variant"].clone(), q[0]["positives_showing"].clone(), q[0]["humans_showing"].clone(), q[0]["score_numerator"].clone()), (json!("curly"), json!(2), json!(0), json!("4")));
        assert_eq!((q[1]["variant"].clone(), q[1]["positives_showing"].clone(), q[1]["humans_showing"].clone(), q[1]["score_numerator"].clone()), (json!("ascii"), json!(0), json!(1), json!("-2")));
        let inp = rep["inputs"].as_array().unwrap();
        assert_eq!(inp[0]["role"], "humans");
        assert_eq!((inp[0]["rows_used"].clone(), inp[0]["rows_zero_weight"].clone(), inp[0]["rows_other_origin"].clone()), (json!(2), json!(1), json!({"mixed": 1, "model_only": 1})));
        assert_eq!((inp[1]["rows_used"].clone(), inp[1]["rows_zero_weight"].clone(), inp[1]["rows_other_origin"].clone()), (json!(2), json!(1), json!({"human_only": 1})));
        assert_eq!(inp[0]["input"]["rows"], 5);
        assert_eq!(inp[0]["input"]["manifest_sha256"], hex::encode(Sha256::digest(std::fs::read(humans.join("MANIFEST.json")).unwrap())));
        assert_eq!(inp[1]["input"]["rows_sha256"], hex::encode(Sha256::digest(std::fs::read(positives.join("rows.jsonl")).unwrap())));
        assert_eq!(rep["input_rule"], INPUT_RULE);
        assert_text_free(&rep, mx);
        // One split may serve both roles (N0's raw-twin Train split holds both).
        let both_dir = split_dir(&tmp.0, "both", &[
            weighted("h1", "\"Plain,\" he said to us.", "human_only", &[], 1.0),
            weighted("p2", p2, "model_only", &[(0, p2.len())], 1.0),
        ], |_| {});
        let r2 = audit_train_splits(std::slice::from_ref(&both_dir), std::slice::from_ref(&both_dir)).unwrap();
        assert_eq!((r2["humans"].clone(), r2["positives"].clone()), (json!(1), json!(1)));
        // A row id at most once per role.
        assert!(audit_train_splits(&[humans.clone(), humans.clone()], std::slice::from_ref(&positives)).is_err());
        // Every row needs a weight.
        let nw = split_dir(&tmp.0, "noweight", &[row("h9", "No weight field here.", "human_only", &[])], |_| {});
        assert!(audit_train_splits(&[nw], std::slice::from_ref(&positives)).is_err());
        assert!(audit_train_splits(&[], std::slice::from_ref(&positives)).is_err());
    }

    #[test]
    fn train_split_reader_refuses_other_inputs() {
        let tmp = TmpDir::new("reader");
        let ok = [weighted("a", "First synthetic row.", "human_only", &[], 1.0), weighted("b", "Second synthetic row.", "human_only", &[], 1.0)];
        let d = split_dir(&tmp.0, "ok", &ok, |_| {});
        let mut n = 0;
        let rec = for_train_split(&d, |_, _| {
            n += 1;
            Ok(())
        })
        .unwrap();
        assert_eq!((n, rec["rows"].clone(), rec["build_id"].clone()), (2, json!(2), json!("b-test")));
        // A non-Train row stops the read before any later line.
        let mut dev = weighted("x", "A row of another split.", "human_only", &[], 1.0);
        dev["split"] = json!("dev-select");
        let mixed = split_dir(&tmp.0, "mixed", &[ok[0].clone(), dev, ok[1].clone()], |_| {});
        let mut seen = Vec::new();
        let err = for_train_split(&mixed, |_, r| {
            seen.push(r["id"].as_str().unwrap().to_string());
            Ok(())
        })
        .unwrap_err();
        assert!(format!("{err:#}").contains("line 2 is not a Train row"), "{err:#}");
        assert_eq!(seen, vec!["a".to_string()]);
        let refuse = |name: &str, tweak: fn(&mut Value)| {
            let d = split_dir(&tmp.0, name, &ok, tweak);
            for_train_split(&d, |_, _| Ok(())).is_err()
        };
        assert!(refuse("sealed", |m| m["sealed"] = json!(true)));
        assert!(refuse("devsplit", |m| m["split"] = json!("dev-iter")));
        assert!(refuse("digest", |m| m["files"]["rows.jsonl"] = json!("00")));
        assert!(refuse("count", |m| m["rows"] = json!(3)));
        assert!(refuse("schema", |m| m["schema"] = json!("slop_ninja_other_v1")));
        assert!(refuse("nocount", |m| m.as_object_mut().unwrap().remove("rows").map(|_| ()).unwrap()));
        assert!(for_train_split(&tmp.0.join("absent"), |_, _| Ok(())).is_err());
    }

    #[test]
    fn twin_rows_take_one_split() {
        let t = human_text();
        let mut h = row("h1", &t, "human_only", &[]);
        let mut o = row("o1", &t, "model_only", &[(0, t.len())]);
        let mut x = row("x1", &t, "human_only", &[]);
        h["split"] = json!("dev-iter-2");
        o["split"] = json!("dev-iter-2");
        x["split"] = json!("test-2");
        let body = |rs: &[&Value]| rs.iter().map(|r| serde_json::to_string(r).unwrap() + "\n").collect::<String>();
        let mut sunk = Vec::new();
        let c = twin_rows(body(&[&h, &o]).as_bytes(), "dev-iter-2", &both(), "abc", |m, p| {
            sunk.push((m["kind"].as_str().unwrap().to_string(), p["probe_of"].as_str().unwrap().to_string()));
            Ok(())
        })
        .unwrap();
        assert_eq!(c.rows, 2);
        assert_eq!(c.twins_by_kind, [("h_inj_c".to_string(), 1usize), ("h_inj_l".to_string(), 1)].into_iter().collect::<std::collections::BTreeMap<_, _>>());
        assert_eq!(c.rows_without_twins_by_origin, [("model_only".to_string(), 1usize)].into_iter().collect::<std::collections::BTreeMap<_, _>>());
        assert_eq!(sunk, vec![("h_inj_c".to_string(), "h1".to_string()), ("h_inj_l".to_string(), "h1".to_string())]);
        // A row of another split stops the run before any later line.
        let mut n = 0;
        let err = twin_rows(body(&[&x, &h]).as_bytes(), "dev-iter-2", &both(), "abc", |_, _| {
            n += 1;
            Ok(())
        })
        .unwrap_err();
        assert!(format!("{err:#}").contains("stopped before reading further"), "{err:#}");
        assert_eq!(n, 0);
        assert!(twin_rows(body(&[&h, &h]).as_bytes(), "dev-iter-2", &both(), "abc", |_, _| Ok(())).is_err(), "duplicate ids");
    }
}
