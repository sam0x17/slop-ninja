//! v1 form-rule harness shared by the byte-identity tests.
//!
//! The including test binds every name below in a `form` module at its crate
//! root: `grammar-cc-human` binds the v1 public paths (`era_norm`,
//! `grammar_a_pools::{text, fineweb, splits}`, `grammar_a_positives::text`
//! and `generation::doc_window_reasons`); `grammar-a-form` binds its own
//! modules. Both must reproduce the golden digests, which were recorded
//! from the v1 code before the form rules moved.
//!
//! Every fixture is synthetic. The golden file holds case names and SHA-256
//! digests only.
use crate::form::*;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::Path;

pub struct Fixture {
    pub name: &'static str,
    pub text: &'static str,
}

/// Parent-like texts: excerpt layout and every way out of it.
pub const PARENTS: &[Fixture] = &[
    Fixture { name: "curly_em_closed", text: "\u{201C}We should leave before the tide turns,\u{201D} the warden told the group on the pier.\n\nThe boats were tied up early that evening\u{2014}nobody wanted to risk the channel after dark.\n\nBy morning the water was calm again, and the ferry ran on time from 7\u{2013}9 as usual\u{2026}" },
    Fixture { name: "ascii_hyphen_spaced", text: "\"It's fine,\" said the clerk - mostly to himself - as he stamped the forms.\n\nThe office opened at 9-5 on weekdays, and the queue grew longer every hour... nobody complained aloud.\n\nHe filed the last folder at noon and went out for a sandwich at the corner shop." },
    Fixture { name: "quote_tie_em_spaced", text: "\u{201C}Maybe,\u{201D} he said, and then \"perhaps\" again \u{2014} twice \u{2014} before the meeting began.\n\nThe chair\u{2019}s notes were short, and the minutes didn\u{2019}t mention the vote at all that day." },
    Fixture { name: "double_hyphen", text: "The plan -- as far as anyone knew -- was to repaint the hall before the spring fair.\n\nVolunteers signed up for shifts of two--three hours each, mostly on weekends in April." },
    Fixture { name: "en_spaced_ranges", text: "The lecture ran long \u{2013} as lectures do \u{2013} and the room emptied slowly after six.\n\nPages 10\u{2013}12 were assigned for the following week, with a short quiz on chapters 3\u{2013}4." },
    Fixture { name: "single_newlines_trailing", text: "The garden needs more light than it gets in the spring.  \nWe moved the pots twice this summer to find a better spot.\nThe tomatoes still did poorly, but the herbs were fine all year.\n" },
    Fixture { name: "list_lines", text: "Things to bring on the walk:\n- a coat for the evening chill\n- two pairs of thick socks\n* a small torch\n1. Check the timetable before leaving home.\n2) Lock the back door." },
    Fixture { name: "title_line", text: "A Walk by the River\n\nWe walked along the river path until the light began to fade behind the hills." },
    Fixture { name: "url", text: "More details are posted at www.example.org for anyone who wants to read the full schedule.\n\nThe meeting itself is open to everyone in the district this year, as before." },
    Fixture { name: "markup", text: "The page had leftover [[link]] markup and a {{template}} in the middle of the text.\n\nIt also used __double underscores__ and **bold** words for emphasis in the second part." },
    Fixture { name: "non_ascii", text: "Caf\u{e9} owners in the old quarter said the d\u{e9}cor had not changed in thirty years at all.\n\n\u{41f}\u{440}\u{438}\u{432}\u{435}\u{442} \u{43c}\u{438}\u{440} \u{43d}\u{430} \u{434}\u{43e}\u{440}\u{43e}\u{433}\u{435} this mixed line has a few words." },
    Fixture { name: "damaged", text: "A broken \u{FFFD} character sits in this line of otherwise ordinary prose text here.\n\nA control \u{7} bell appears in this second paragraph of the sample text too." },
    Fixture { name: "gap", text: "The rendered paragraph had a gap \u{E000} where an image used to be in the article body." },
    Fixture { name: "long_quotes", text: "The sign read \u{201C}no swimming after dark under any circumstances at this beach\u{201D} in large letters.\n\nShe said \"keep the gate closed at all times when the dogs are out\" twice to us." },
    Fixture { name: "abbreviations", text: "Dr. Hale met Mr. Ortiz at 3 p.m. on Jan. 4 near the U.S. border post. They spoke briefly.\n\nJ. Ortiz later wrote that the talk, e.g. about permits, went well. Prof. Lin agreed with him." },
    Fixture { name: "ellipsis_mix", text: "Well... perhaps. Or perhaps not\u{2026} The answer was never clear.... Four dots end this one.\n\nShe waited\u{2026} and waited... and finally left the station alone that night." },
    Fixture { name: "dates_codes", text: "The memo dated 2010-05-01 mentioned COVID-19 and a well--known plan for 1990-1995 budgets.\n\nScores were 3-4 and (12-14), with ranges like [5-6] and 7-8, noted in the margin." },
    Fixture { name: "crlf_tabs_nbsp", text: "First line with\ta tab and a carriage return at its end.\r\nSecond line, still in the same paragraph of the note.\r\n\r\nThird line in a new paragraph with\u{a0}a no-break space inside it." },
    Fixture { name: "angle_brackets", text: "The value was p < 0.05 in the first test and > 0.1 in the second test run here." },
    Fixture { name: "boilerplate", text: "Please read our privacy policy before you continue reading this article on the site.\n\nContact the editor at someone@example.com with corrections or other notes about it." },
    Fixture { name: "short", text: "Too short here.\n\nAlso short." },
    Fixture { name: "empty", text: "" },
    Fixture { name: "whitespace_only", text: " \n\n \t \n" },
    Fixture { name: "elisions", text: "'Tis the season, and 'twas a long one; rock 'n' roll played 'til late in the '90s.\n\nThey'd say 'cause it's late, but 'em folks never left early on a Saturday night." },
    Fixture { name: "em_variants", text: "A double em\u{2014}\u{2014}dash, a lead \u{2014}dash, a horizontal bar\u{2015}here, and a spaced \u{2014} one.\n\nTrailing dash at the end \u{2014}" },
    Fixture { name: "emphasis_odd", text: "A **bold** word and an unpaired ** marker and __under__ plus __ another one here." },
    Fixture { name: "blank_runs", text: "\n\nOpening blank lines come first in this one, then the text of the first paragraph.\n\n\n\nFour blank lines sit before this second paragraph of plain prose.\n\n" },
    Fixture { name: "single_quotes_ascii", text: "The 'quiet' room was 'reserved' for the 'senior' staff, or so the 'rules' said.\n\nIt's the same every year, and nobody's surprised by it anymore at all." },
];

/// Child-like texts: what local generators return.
pub const CHILDREN: &[Fixture] = &[
    Fixture { name: "trailing_space_quotes", text: "The tide had turned by the time the boats came in.  \nNobody on the pier said a word about the \"storm\" coming in from the west.\nThe crew waited below - talking about the weather and the plans for the evening ahead..." },
    Fixture { name: "markdown_list", text: "**Key points:**\n- The ferry ran late.\n- The harbour was quiet.\n\nOverall, it was a calm day on the water \u{2014} mostly." },
    Fixture { name: "heading", text: "# A Day at the Harbour\n\nThe ferry left at dawn.\nThe gulls followed it out to sea for a while, then turned back to land. It was a quiet trip. Nobody minded the wait at all." },
    Fixture { name: "curly_mix", text: "\u{201C}It\u{2019}s late,\u{201D} she said. \u{2018}Very late.\u{2019} We left at 9\u{2013}10 p.m. and walked home\u{2026} The streets were empty and the shops had closed hours before." },
    Fixture { name: "short", text: "Short." },
    Fixture { name: "empty", text: "" },
    Fixture { name: "unterminated", text: "Line one without an end\nLine two also without an end" },
    Fixture { name: "abbreviations", text: "Dr. Smith arrived. He sat down. e.g. this is a clause. The end came soon after that, around 5 p.m. Then everyone left. Mr. Jones stayed. J. Doe waved." },
    Fixture { name: "dashes_all", text: "It rained\u{2014}hard. Then \u{2014} briefly \u{2013} it cleared - mostly -- or so. In 1990-1995 and 3\u{2013}4 days... wait\u{2026} 2010-05-01 COVID-19 well--known." },
    Fixture { name: "emphasis", text: "A **very** important __note__ about the plan, which was **simple** and clear to everyone involved in it." },
    Fixture { name: "single_quotes", text: "'Twas the night before the fair, and the 'old' barn was full. Rock 'n' roll played 'til dawn in the '80s style. They'd come 'cause of 'em." },
    Fixture { name: "crlf_nbsp", text: "The report came in late.\r\nIt was\u{a0}long and\tdetailed, with many tables.\r\n\r\nThe board read it twice before the vote on Friday afternoon." },
];

fn digest(s: &str) -> String {
    hex::encode(Sha256::digest(s.as_bytes()))
}

fn site_json(s: &Site) -> Value {
    match s {
        Site::Dash(a, b, d) => json!(["dash", a, b, d, d.render()]),
        Site::Range(a, b, r) => json!(["range", a, b, r]),
        Site::Ellipsis(a, b, e) => json!(["ellipsis", a, b, e]),
    }
}

fn para_json(p: &Para) -> Value {
    json!({"text": p.text, "prose": p.prose, "reason": p.reason})
}

fn normalised_json(t: &str, conv: &Conventions, st: &Structure) -> Value {
    let n = normalise_child(t, conv, st);
    json!({"text": n.text, "changes": n.changes, "layout": n.layout, "line_reasons": n.line_reasons})
}

fn choice<K>(value: Option<K>) -> Choice<K> {
    Choice { value, source: "parent" }
}

/// Hand-built conventions that reach every rendering branch.
fn fixed_conventions() -> Vec<(&'static str, Conventions)> {
    let mk = |d, s, dash, range, ell| Conventions { double: choice(d), single: choice(s), dash: choice(dash), range: choice(range), ellipsis: choice(ell) };
    let mut out = vec![("none", mk(None, None, None, None, None))];
    for (i, dash) in DASHES.iter().enumerate() {
        let q = if i % 2 == 0 { Quote::Curly } else { Quote::Ascii };
        let r = RANGES[i % 2];
        let e = ELLIPSES[(i / 2) % 2];
        out.push((["em_closed", "em_spaced", "en_spaced", "hyphen_spaced", "double_closed", "double_spaced"][i], mk(Some(q), Some(q), Some(*dash), Some(r), Some(e))));
    }
    out.push(("curly_double_ascii_single", mk(Some(Quote::Curly), Some(Quote::Ascii), None, Some(Range::En), None)));
    out.push(("ascii_double_curly_single", mk(Some(Quote::Ascii), Some(Quote::Curly), Some(Dash::EmClosed), None, Some(Ellipsis::Dots))));
    out
}

/// Spans of a parent to replace: each blank-line paragraph, and the run
/// from the first sentence break to the last (ASCII offsets, so char
/// boundaries hold).
fn spans_of(p: &str) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut start = 0;
    for (i, _) in p.match_indices("\n\n") {
        out.push((start, i));
        start = i + 2;
    }
    out.push((start, p.len()));
    if let (Some(a), Some(b)) = (p.find(". "), p.rfind(". "))
        && a + 2 <= b + 1
    {
        out.push((a + 2, b + 1));
    }
    out.retain(|&(s, e)| s <= e && p.is_char_boundary(s) && p.is_char_boundary(e));
    out
}

/// A long synthetic child: varied sentence lengths, one line.
fn long_child() -> String {
    let mut s = String::new();
    for i in 0..40 {
        let n = 3 + (i * 7) % 13;
        let words: Vec<String> = (0..n).map(|k| format!("w{}x{}", i, k)).collect();
        s.push_str(&format!("{}{} ", if i % 5 == 0 { "And " } else { "The " }, words.join(" ")));
        s.pop();
        s.push_str(if i % 9 == 0 { "? " } else { ". " });
    }
    s.trim_end().to_string()
}

/// A document of `n` whitespace words in blank-line paragraphs of 18 words.
fn doc_of(n: usize) -> String {
    let words: Vec<String> = (0..n).map(|k| if k % 18 == 17 || k + 1 == n { format!("w{k}.") } else { format!("w{k}") }).collect();
    words.chunks(18).map(|c| c.join(" ")).collect::<Vec<_>>().join("\n\n")
}

/// Every case as (name, canonical JSON line), in a fixed order.
pub fn run() -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    let mut emit = |case: String, v: Value| out.push((case, serde_json::to_string(&v).expect("json")));

    emit("constants".into(), json!({
        "norm_id": NORM_ID, "profile_v1": PROFILE_V1, "profile_raw": PROFILE_RAW,
        "length_window_id": LENGTH_WINDOW_ID, "length_window": LENGTH_WINDOW, "key_scope": KEY_SCOPE,
        "excerpt_layout_id": EXCERPT_LAYOUT_ID, "min_paragraph_words": MIN_PARAGRAPH_WORDS,
        "abbreviations": &ABBREVIATIONS[..], "elisions": &ELISIONS[..], "boilerplate": BOILERPLATE,
        "gap": GAP.to_string(), "min_words": MIN_WORDS, "max_words": MAX_WORDS,
        "dashes": DASHES.iter().map(|d| json!([d, d.render()])).collect::<Vec<_>>(),
        "ranges": RANGES, "ellipses": ELLIPSES,
    }));

    for p in [Profile::Raw, Profile::NormV1, Profile::CohortV1] {
        emit(format!("profile/{}", p.id()), json!({
            "id": p.id(), "parse": Profile::parse(p.id()).map(|x| x.id()).map_err(|e| e.to_string()),
            "normalises": p.normalises(), "window": p.window(), "excludes": p.excludes(), "record": p.record(),
            "check_prior_none": p.check_prior(None).map_err(|e| e.to_string()),
            "check_prior_some": p.check_prior(Some(Path::new("prior.json"))).map_err(|e| e.to_string()),
        }));
    }
    emit("profile/unknown".into(), json!({"parse": Profile::parse("layout_v0").map(|x| x.id()).map_err(|e| e.to_string()),
        "default": Profile::DEFAULT.id()}));

    let all: Vec<&Fixture> = PARENTS.iter().chain(CHILDREN.iter()).collect();
    for (role, f) in PARENTS.iter().map(|f| ("parent", f)).chain(CHILDREN.iter().map(|f| ("child", f))) {
        let t = f.text;
        let case = |k: &str| format!("text/{role}/{}/{k}", f.name);
        let lines: Vec<&str> = t.split('\n').collect();
        emit(case("sites"), json!(lines.iter().map(|l| sites(l).iter().map(site_json).collect::<Vec<_>>()).collect::<Vec<_>>()));
        let c = counts(t);
        emit(case("counts"), json!({"counts": c, "own": own(&c)}));
        emit(case("lines"), json!(lines.iter().map(|l| json!({
            "classify": para_json(&classify(l, false)), "classify_repeated": para_json(&classify(l, true)),
            "markup": para_json(&Para::classify(squash(l), Screen::MARKUP)), "xml": para_json(&Para::classify(squash(l), Screen::XML)),
            "no_quotes": prose_reason(l, Screen { quotes: false, angle_brackets: true }),
            "list_like": list_like(l), "boilerplate": boilerplate(l), "ends_sentence": ends_sentence(l), "has_url": has_url(l),
            "damaged": damaged(l), "ascii_letter_ratio_bits": ascii_letter_ratio(l).to_bits(), "long_quotation": long_quotation(l),
            "words": words(l), "gwords": gwords(l), "normalized": normalized(l), "squash": squash(l),
        })).collect::<Vec<_>>()));
        emit(case("sentences"), json!(sentences(t).iter().map(|s| json!([s.start, s.end, s.punctuated])).collect::<Vec<_>>()));
        emit(case("text_hashes"), json!({"h64": h64(t.as_bytes()), "long_quotation": long_quotation(t), "words": words(t)}));
        let lay = excerpt_layout(t);
        emit(case("excerpt_layout"), json!({"text": lay, "fixed_point": lay == t}));
        emit(case("paragraph_screen"), json!({"raw": paragraph_screen(t, None), "laid_out": paragraph_screen(&lay, None),
            "spans": spans_of(t), "on_spans": paragraph_screen(t, Some(spans_of(t).as_slice()))}));
        for p in [Profile::Raw, Profile::NormV1, Profile::CohortV1] {
            let (et, rec, why) = every_class(t, p);
            emit(case(&format!("every_class/{}", p.id())), json!({"text": et, "record": rec, "reasons": why}));
        }
        emit(case("structure"), json!({"whole": Structure::whole(t),
            "spans": spans_of(t).iter().map(|&(s, e)| Structure::span(t, s, e)).collect::<Vec<_>>()}));
        for (cname, conv) in fixed_conventions() {
            emit(case(&format!("normalise/self/{cname}")), normalised_json(t, &conv, &Structure::whole(t)));
        }
    }

    // Priors over every parent, and the empty and tied priors.
    let prior_all = Prior::of(PARENTS.iter().map(|f| f.text));
    let prior_tie = Prior {
        parents: 4,
        quotes: BTreeMap::from([(Quote::Curly, 2), (Quote::Ascii, 2)]),
        dash: BTreeMap::from([(Dash::EmClosed, 1), (Dash::HyphenSpaced, 1), (Dash::DoubleSpaced, 1)]),
        range: BTreeMap::from([(Range::En, 1)]),
        ellipsis: BTreeMap::new(),
    };
    let priors = [("all", prior_all.clone()), ("default", Prior::default()), ("tie", prior_tie)];
    emit("prior/all".into(), json!(prior_all));
    emit("prior/all_texts".into(), json!(Prior::of(all.iter().map(|f| f.text))));
    let seeds = ["cc-era-parent-norm-20261031", "x"];
    for p in PARENTS {
        let c = counts(p.text);
        for (pname, prior) in &priors {
            for seed in seeds {
                for doc in ["doc-a", "parent-7:mixed", p.name] {
                    emit(format!("resolve/{}/{pname}/{seed}/{doc}", p.name), json!(resolve(&c, prior, seed, doc)));
                }
            }
        }
    }
    // Keyed draws across many documents with no evidence at all.
    let none = counts("Plain words only here.");
    emit("resolve/no_evidence/sweep".into(), json!((0..64).map(|k| {
        let r = resolve(&none, &priors[0].1, "s", &format!("doc-{k}"));
        json!([r.double.value, r.single.value, r.dash.value, r.range.value, r.ellipsis.value, r.double.source])
    }).collect::<Vec<_>>()));

    // Children normalised against each parent: mirror and every span.
    for (i, p) in PARENTS.iter().enumerate() {
        let c = counts(p.text);
        let conv = resolve(&c, &priors[0].1, seeds[0], p.name);
        for (j, ch) in CHILDREN.iter().enumerate() {
            if (i + j) % 3 != 0 && j != i % CHILDREN.len() {
                continue;
            }
            emit(format!("child/{}/{}/whole", p.name, ch.name), normalised_json(ch.text, &conv, &Structure::whole(p.text)));
            for (s, e) in spans_of(p.text) {
                emit(format!("child/{}/{}/span/{s}-{e}", p.name, ch.name), normalised_json(ch.text, &conv, &Structure::span(p.text, s, e)));
            }
        }
        let lc = long_child();
        emit(format!("child/{}/long/whole", p.name), normalised_json(&lc, &conv, &Structure::whole(p.text)));
        let st = Structure { fractions: vec![0.1, 0.25, 0.5, 0.52, 0.9], pre_words: 3, post_words: 2 };
        emit(format!("child/{}/long/fractions", p.name), normalised_json(&lc, &conv, &st));
    }

    // Break placement on synthetic sentence grids.
    let grids: Vec<(Vec<usize>, Vec<f64>, usize, usize)> = vec![
        (vec![5; 10], vec![0.3, 0.7], 0, 0),
        (vec![5; 10], vec![0.05], 0, 0),
        (vec![5, 5, 5, 5], vec![0.3, 0.5], 0, 0),
        (vec![5, 5, 5], vec![0.3, 0.5], 0, 0),
        (vec![5; 10], vec![0.1], 20, 0),
        (vec![3, 9, 2, 14, 1, 8, 8, 4, 12, 6, 7, 2], vec![0.2, 0.4, 0.6, 0.8], 0, 0),
        (vec![3, 9, 2, 14, 1, 8, 8, 4, 12, 6, 7, 2], vec![0.33, 0.34, 0.35], 4, 4),
        (vec![1; 30], vec![0.5], 0, 7),
        (vec![], vec![0.5], 0, 0),
        (vec![40], vec![0.5], 0, 0),
        (vec![8, 8], vec![0.5], 0, 0),
        (vec![8, 8], vec![], 0, 0),
        // Exact ties (binary fractions), for the earliest-on-ties rules.
        // Final choice: boundaries at 0.25 and 0.75 are equally far from 0.5.
        (vec![16, 32, 16], vec![0.5], 0, 0),
        // Inner step: breaks after 1 or 2 sentences cost the same for 0.1875.
        (vec![8; 8], vec![0.1875, 0.75], 0, 0),
        // Drop: three breaks cannot fit; every target's two paragraphs are 0.5.
        (vec![4; 7], vec![0.25, 0.5, 0.75], 0, 0),
    ];
    for (k, (w, f, pre, post)) in grids.into_iter().enumerate() {
        let st = Structure { fractions: f, pre_words: pre, post_words: post };
        let all_ok = vec![true; w.len() + 1];
        let mut some: Vec<bool> = (0..=w.len()).map(|j| j % 3 != 1).collect();
        if let Some(x) = some.first_mut() {
            *x = false;
        }
        emit(format!("breaks/{k}"), json!({"all": place_breaks(&w, &all_ok, &st), "some": place_breaks(&w, &some, &st)}));
    }

    // Keyed ranks.
    emit("rank".into(), json!([
        rank("s", "scope", "key"), rank("", "", ""), rank(seeds[0], KEY_SCOPE, "doc-a:quotes"),
        rank(seeds[0], KEY_SCOPE, "doc-a:dash"), rank("seed\u{0}x", "a", "b"), rank("seed", "fineweb_row", "<urn:uuid:0000>"),
    ]));

    // The length window, on counts and on whole documents.
    emit("window/reasons".into(), json!((0..=600).step_by(1).filter(|n| n % 50 == 0 || (148..=152).contains(n) || (448..=452).contains(n))
        .map(|n| json!([n, window_reasons(n, LENGTH_WINDOW), window_reasons(n, (10, 20))])).collect::<Vec<_>>()));
    for n in [0usize, 1, 144, 149, 150, 151, 162, 449, 450, 451, 468, 600] {
        let d = doc_of(n);
        for p in [Profile::NormV1, Profile::CohortV1] {
            let (_, rec, why) = every_class(&d, p);
            emit(format!("window/doc_{n}/{}", p.id()), json!({"record": rec, "reasons": why}));
        }
        let bad = format!("{d}\n\nRead more at www.example.com today");
        let (_, rec, why) = every_class(&bad, Profile::CohortV1);
        emit(format!("window/doc_{n}/bad"), json!({"record": rec, "reasons": why}));
    }
    out
}

/// The golden group of a case: its fixture (`text/<role>/<name>`) or area.
fn group(case: &str) -> String {
    let n = if case.starts_with("text/") { 3 } else { 2 };
    case.split('/').take(n).collect::<Vec<_>>().join("/")
}

/// One golden line per group (case count and the digest of its case names
/// and lines in order), then the same over every case.
pub fn golden(cases: &[(String, String)]) -> String {
    let mut groups: Vec<(String, usize, Sha256)> = Vec::new();
    let mut all = Sha256::new();
    for (case, line) in cases {
        let g = group(case);
        if groups.last().is_none_or(|x| x.0 != g) {
            assert!(groups.iter().all(|x| x.0 != g), "group {g} is not contiguous");
            groups.push((g, 0, Sha256::new()));
        }
        let last = groups.last_mut().expect("group");
        last.1 += 1;
        for h in [&mut last.2, &mut all] {
            h.update(case.as_bytes());
            h.update(b"\t");
            h.update(digest(line).as_bytes());
            h.update(b"\n");
        }
    }
    let mut s = String::new();
    for (g, n, h) in groups {
        s.push_str(&format!("{g}\t{n}\t{}\n", hex::encode(h.finalize())));
    }
    s.push_str(&format!("ALL\t{}\t{}\n", cases.len(), hex::encode(all.finalize())));
    s
}

/// Compares against the golden file; `FORM_V1_GOLDEN_WRITE=<path>` writes it instead.
pub fn check(expected: &str) {
    let cases = run();
    let got = golden(&cases);
    if let Ok(path) = std::env::var("FORM_V1_GOLDEN_WRITE") {
        std::fs::write(&path, &got).expect("write golden");
        return;
    }
    let want: BTreeMap<&str, &str> = expected.lines().filter_map(|l| l.split_once('\t')).collect();
    let have: BTreeMap<&str, &str> = got.lines().filter_map(|l| l.split_once('\t')).collect();
    let differ: Vec<&str> = want.keys().chain(have.keys()).filter(|k| want.get(*k) != have.get(*k)).copied().collect();
    assert!(differ.is_empty(), "{} v1 form cases differ from the golden digests (first: {:?})", differ.len(), &differ[..differ.len().min(12)]);
    assert_eq!(got, expected, "golden order or count differs");
}
