//! Parity of the two pieces of profile v2 that `grammar-a-form` copies from
//! `grammar-a-positives` (which cannot be its dependency): the spliced
//! generator's assembly rules (`profile::assemble_item` against
//! `splice::assemble_parts`) and the round-2 plan key (V21) behind
//! `profile::norm_prior_seed`. Synthetic strings only.
use grammar_a_form::profile::{self, Part};
use grammar_a_positives::{span_ops, splice};

fn positives(parent: &str, parts: &[Part], reps: &[&str]) -> Result<(String, Vec<(usize, usize)>), String> {
    let ps: Vec<splice::Part> = parts.iter().map(|p| splice::Part { parent_span: p.span, insert_sep: p.insert_sep }).collect();
    splice::assemble_parts(parent, &ps, reps).map(|a| (a.text, a.edit_spans)).map_err(|e| e.to_string())
}

fn form(parent: &str, parts: &[Part], reps: &[&str]) -> Result<(String, Vec<(usize, usize)>), String> {
    profile::assemble_item(parent, parts, reps).map(|a| (a.text, a.edit_spans)).map_err(str::to_string)
}

fn boundaries(s: &str) -> Vec<usize> {
    (0..=s.len()).filter(|&i| s.is_char_boundary(i)).collect()
}

#[test]
fn assemble_item_matches_assemble_parts() {
    let parents = ["Alpha one. Beta two.\n\nGamma three.", "A \u{2014}b\u{a0}c. D\u{1c}e f.", "One. Two.  Three"];
    let reps = ["New text.", " padded", "x", "two words", "", "tail \u{1f}", "\u{201C}q\u{201D}"];
    let seps = [None, Some(" "), Some("\n\n")];
    let mut cases = 0usize;
    let mut oks = 0usize;
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
                        let r = ["First new.", "second"];
                        let (x, y) = (positives(parent, &parts, &r), form(parent, &parts, &r));
                        assert_eq!(x, y, "parent {parent:?} spans {:?} {:?}", (s1, e1), (s2, e2));
                        oks += x.is_ok() as usize;
                        cases += 1;
                    }
                }
            }
        }
    }
    assert_eq!(form("p", &[], &[]), positives("p", &[], &[]));
    assert!(cases > 10_000 && oks > 100, "{cases} cases, {oks} assembled");
}

#[test]
fn norm_prior_seed_is_the_v21_plan_key() {
    assert_eq!(profile::R2_PLAN_KEY_PREFIX, span_ops::R2_PLAN_KEY_PREFIX);
    for p in ["train2", "dev_iter2", "cal2", "select2", "test2", "dev_iter2_mock", ""] {
        assert_eq!(profile::norm_prior_seed(p), span_ops::r2_key(profile::NORM_PRIOR_KEY, p), "{p}");
    }
    assert_ne!(profile::norm_prior_seed("train2"), profile::norm_prior_seed("cal2"));
}
