//! Parity of the two pieces of profile v2 that `grammar-a-form` copies from
//! `grammar-a-positives` (which cannot be its dependency): the spliced
//! generator's assembly rules for laid-out replacements
//! (`profile::assemble_item` against `splice::assemble_laid_parts`, with
//! protocol A2 amendment 42 item 42.2's boundary break and its restated
//! human-characters check), the raw responses' rules that amendment 42 does
//! not change (`splice::assemble_parts`: no trailing whitespace) and the
//! round-2 plan key (V21) behind `profile::norm_prior_seed`. Synthetic
//! strings only.
use grammar_a_form::profile::{self, Part};
use grammar_a_positives::{generation, span_ops, splice};

fn parts_of<'a>(parts: &[Part<'a>]) -> Vec<splice::Part<'a>> {
    parts.iter().map(|p| splice::Part { parent_span: p.span, insert_sep: p.insert_sep }).collect()
}

fn positives(parent: &str, parts: &[Part], reps: &[&str]) -> Result<(String, Vec<(usize, usize)>), String> {
    splice::assemble_laid_parts(parent, &parts_of(parts), reps).map(|a| (a.text, a.edit_spans)).map_err(|e| e.to_string())
}

fn raw(parent: &str, parts: &[Part], reps: &[&str]) -> Result<(String, Vec<(usize, usize)>), String> {
    splice::assemble_parts(parent, &parts_of(parts), reps).map(|a| (a.text, a.edit_spans)).map_err(|e| e.to_string())
}

fn trailing_ws(r: &str) -> bool {
    r.trim_end_matches(|c: char| c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)) != r
}

fn form(parent: &str, parts: &[Part], reps: &[&str]) -> Result<(String, Vec<(usize, usize)>), String> {
    profile::assemble_item(parent, parts, reps).map(|a| (a.text, a.edit_spans)).map_err(str::to_string)
}

fn boundaries(s: &str) -> Vec<usize> {
    (0..=s.len()).filter(|&i| s.is_char_boundary(i)).collect()
}

#[test]
fn assemble_item_matches_assemble_parts() {
    let parents = ["Alpha one. Beta two.\n\nGamma three.", "A \u{2014}b\u{a0}c. D\u{1c}e f.", "One. Two.  Three", "Uno dos.\t Tres\n cuatro"];
    // amendment 29 item 29.1's leading line break and lead (accepted), and
    // leading whitespace of other forms (refused); then amendment 42 item
    // 42.2's trailing boundary break (a run, one line break, a lead: accepted
    // where the run is the parent's whole run before a parent word), with a
    // leading break too, and trailing whitespace of other forms (refused)
    let reps = [
        "New text.", " padded", "x", "two words", "", "tail \u{1f}", "\u{201C}q\u{201D}", "\n  lead text.", "\n\tx", "\n\nnot one break", "\n \nnor this",
        "x \n", "x  \n", "x \n  ", "x\t \n", "x\t\n", "\n  both. \n", "x \n\n", "x\n", "x\n ", "x ", "x\u{1f} \n", "x \n \n",
    ];
    let seps = [None, Some(" "), Some("\n\n")];
    let mut cases = 0usize;
    let mut oks = 0usize;
    let mut tails = 0usize;
    for parent in parents {
        let b = boundaries(parent);
        // Every single edit: each span (insertions included), separator and replacement.
        for &s in &b {
            for &e in &b {
                for rep in reps {
                    for sep in seps {
                        let parts = [Part { span: (s, e), insert_sep: sep }];
                        let (x, y) = (positives(parent, &parts, &[rep]), form(parent, &parts, &[rep]));
                        assert_eq!(x, y, "parent {parent:?} span {:?} sep {sep:?} rep {rep:?}", (s, e));
                        // raw responses: unchanged, no trailing whitespace
                        let z = raw(parent, &parts, &[rep]);
                        if trailing_ws(rep) {
                            assert!(z.is_err(), "parent {parent:?} span {:?} rep {rep:?}", (s, e));
                        } else {
                            assert_eq!(z, x, "parent {parent:?} span {:?} sep {sep:?} rep {rep:?}", (s, e));
                        }
                        if let Ok((t, sp)) = &x {
                            tails += usize::from(trailing_ws(rep));
                            // the final span ends at the edit's last word
                            assert!(!t[sp[0].0..sp[0].1].ends_with(char::is_whitespace));
                            // profile::splice and generation::assemble_laid
                            // give the same document for a span
                            // (on parents without U+001C..U+001F, which only
                            // Python reads as whitespace)
                            if s < e && sep.is_none() && !parent.contains(|c: char| ('\u{1c}'..='\u{1f}').contains(&c)) {
                                assert_eq!(profile::splice(parent, (s, e), rep).as_ref(), Ok(t), "{parent:?} {:?} {rep:?}", (s, e));
                                assert_eq!(generation::assemble_laid(parent, &[(s, e)], &[rep]).map(|x| x.0).ok().as_ref(), Some(t));
                            }
                        }
                        oks += x.is_ok() as usize;
                        cases += 1;
                    }
                }
            }
        }
        // Two edits over a coarser grid, in both orders.
        let grid: Vec<usize> = b.iter().copied().step_by(3).collect();
        for &s1 in &grid {
            for &e1 in grid.iter().filter(|&&e| e >= s1) {
                for &s2 in &grid {
                    for &e2 in grid.iter().filter(|&&e| e >= s2) {
                        let parts = [Part { span: (s1, e1), insert_sep: Some(" ") }, Part { span: (s2, e2), insert_sep: Some("\n\n") }];
                        for r in [["First new.", "second"], ["First new. \n", "second"], ["First new.", "\n second \n  "]] {
                            let (x, y) = (positives(parent, &parts, &r), form(parent, &parts, &r));
                            assert_eq!(x, y, "parent {parent:?} spans {:?} {:?} {r:?}", (s1, e1), (s2, e2));
                            oks += x.is_ok() as usize;
                            cases += 1;
                        }
                    }
                }
            }
        }
    }
    assert_eq!(form("p", &[], &[]), positives("p", &[], &[]));
    assert!(cases > 10_000 && oks > 100 && tails > 20, "{cases} cases, {oks} assembled, {tails} with a boundary break");
    // a leading line break and lead stay outside the final span
    let (t, sp) = form("One. Two.", &[Part { span: (5, 9), insert_sep: None }], &["\n  Deux."]).unwrap();
    assert_eq!((t.as_str(), &t[sp[0].0..sp[0].1]), ("One. \n  Deux.", "Deux."));
}

#[test]
fn the_boundary_break_and_its_restated_check() {
    // Amendment 42 item 42.2 on one span and on an insertion: the run the
    // replacement keeps is the parent's, then one line break and a lead; the
    // parent resumes at its first non-whitespace byte; the final span ends
    // at the last word. Every assembler gives the same document and errors.
    let p = "One two.  Three four.";
    let span = [Part { span: (4, 8), insert_sep: None }];
    let ok = form(p, &span, &["deux. \u{20}\n  "]).unwrap();
    assert_eq!(ok.0, "One deux.  \n  Three four.");
    assert_eq!(&ok.0[ok.1[0].0..ok.1[0].1], "deux.");
    assert_eq!(positives(p, &span, &["deux.  \n  "]), Ok(ok.clone()));
    assert_eq!(profile::splice(p, (4, 8), "deux.  \n  ").as_deref(), Ok(ok.0.as_str()));
    // a byte of the run changed, the run cut short (a parent space after
    // it), or the run longer than the parent's: the human characters changed
    for bad in ["deux.\t \n", "deux. \n", "deux.   \n"] {
        assert_eq!(form(p, &span, &[bad]).unwrap_err(), "human characters changed", "{bad:?}");
        assert_eq!(positives(p, &span, &[bad]).unwrap_err(), "human characters changed", "{bad:?}");
        assert_eq!(profile::splice(p, (4, 8), bad).unwrap_err(), "human characters changed", "{bad:?}");
        assert_eq!(generation::assemble_laid(p, &[(4, 8)], &[bad]).unwrap_err().to_string(), "human characters changed", "{bad:?}");
    }
    // no parent word after the run on its line: the boundary break cannot
    // exist there
    let q = "One two.  \nThree four.";
    assert_eq!(form(q, &span, &["deux.  \n"]).unwrap_err(), "human characters changed");
    assert_eq!(positives(q, &span, &["deux.  \n"]).unwrap_err(), "human characters changed");
    // any other trailing whitespace: refused as before, the same error
    for bad in ["deux. ", "deux.\n", "deux.  \n\n", "deux.  \n \n"] {
        assert_eq!(form(p, &span, &[bad]).unwrap_err(), "replacement layout", "{bad:?}");
        assert_eq!(positives(p, &span, &[bad]).unwrap_err(), "replacement layout", "{bad:?}");
        assert_eq!(raw(p, &span, &[bad]).unwrap_err(), "replacement layout", "{bad:?}");
        assert_eq!(profile::splice(p, (4, 8), bad).unwrap_err(), "replacement layout", "{bad:?}");
        assert_eq!(generation::assemble(p, &[(4, 8)], &[bad]).unwrap_err().to_string(), "replacement layout", "{bad:?}");
    }
    // the raw assemblers refuse the boundary form too (amendment 42 does not
    // change them)
    assert_eq!(raw(p, &span, &["deux.  \n  "]).unwrap_err(), "replacement layout");
    assert_eq!(generation::assemble(p, &[(4, 8)], &["deux.  \n  "]).unwrap_err().to_string(), "replacement layout");
    // an insertion whose parent text after it starts with a run, and a
    // start-side and an end-side break on one edit
    let ins = [Part { span: (8, 8), insert_sep: Some(" ") }];
    let (t, sp) = form(p, &ins, &["\n  New one.  \n"]).unwrap();
    assert_eq!(t, "One two. \n  New one.  \nThree four.");
    assert_eq!(&t[sp[0].0..sp[0].1], "New one.");
    assert_eq!(positives(p, &ins, &["\n  New one.  \n"]), Ok((t, sp)));
    // an insertion glued to the parent's next byte has no run to keep
    let glued = [Part { span: (10, 10), insert_sep: Some(" ") }];
    assert_eq!(form(p, &glued, &["new \n"]).unwrap_err(), "human characters changed");
    // a later edit starting where the run ends: no parent word between
    let two = [Part { span: (4, 8), insert_sep: None }, Part { span: (10, 15), insert_sep: None }];
    assert_eq!(form(p, &two, &["deux.  \n", "Trois"]).unwrap_err(), "human characters changed");
    assert_eq!(positives(p, &two, &["deux.  \n", "Trois"]).unwrap_err(), "human characters changed");
    assert_eq!(form(p, &two, &["deux.", "Trois"]).unwrap().0, "One deux.  Trois four.");
}

#[test]
fn norm_prior_seed_is_the_v21_plan_key() {
    assert_eq!(profile::R2_PLAN_KEY_PREFIX, span_ops::R2_PLAN_KEY_PREFIX);
    for p in ["train2", "dev_iter2", "cal2", "select2", "test2", "dev_iter2_mock", ""] {
        assert_eq!(profile::norm_prior_seed(p), span_ops::r2_key(profile::NORM_PRIOR_KEY, p), "{p}");
    }
    assert_ne!(profile::norm_prior_seed("train2"), profile::norm_prior_seed("cal2"));
}
