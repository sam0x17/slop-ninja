//! The echo screen `cc_echo_screen_v1` (protocol A2 §7.2, as amended by
//! amendment 4 item 4.2).
//!
//! A child fails acceptance when it echoes its source:
//! * a mixed child whose generated span equals the parent span it replaces;
//! * a mirror equal to its parent.
//!
//! **Two checks.** Every comparison runs two checks on the same pair of
//! spans, and it echoes when **either** matches:
//! * (a) `raw`: the raw response span (the generated text as the raw twin
//!   splices it, before normalisation) against the raw source span (the
//!   parent span it replaces, or the whole parent for a mirror);
//! * (b) `rendered`: the same two spans after `render`, the child
//!   normaliser's emphasis stripping and its Conventions-based quote, dash,
//!   range and ellipsis rendering, applied alike to both.
//!
//! Check (b) catches a copy whose glyphs or emphasis normalisation would
//! change (a minority-convention quote or dash, paired emphasis), and a
//! response that differs from its source only in those; check (a) catches a
//! verbatim copy whatever rendering does to it.
//!
//! "Matches" is byte-identical, or identical after whitespace folding.
//! Folding compares the sequences of whitespace-separated tokens (Unicode
//! `White_Space`, as `str::split_whitespace`): two texts fold equal exactly
//! when `text::normalized` maps them to the same string, so runs, leading
//! and trailing whitespace, line breaks, tabs and NBSP are ignored, while
//! case, punctuation and every other character still count.
//!
//! **The item verdict** is `screen_item`, the one entry point for callers. A
//! mirror is one comparison. A mixed child is screened edit by edit (every
//! edit's generated text is a generated span of §7.2), and the item fails when
//! any edit echoes. A mixed child with no edit fails closed. An insertion
//! (`s == e`) is screened against the empty span it replaces, so it echoes
//! exactly when it adds no token, raw or rendered; it is recorded under its
//! own target, `insertion`, so span echo counts exclude it. A span that is
//! not a character-boundary range of the parent fails both checks closed.
//!
//! The verdict belongs to the item, so a rejected child fails for both twins
//! (the normalised child and its raw twin) and is regenerated like any other
//! acceptance failure. Records hold the screen id, each comparison's target
//! and both checks' outcomes, and the item outcome, never text. Check (a)
//! allocates nothing; check (b) renders each side once, linear in its input;
//! a verdict holds one entry per edit.
use crate::conventions::{Changes, Conventions, render_quotes, render_sites};
use crate::layout::{layout_lines, strip_emphasis};
use serde::Serialize;
use serde_json::{Value, json};

pub const ECHO_SCREEN_ID: &str = "cc_echo_screen_v1";
/// The reason for a mixed child with no edit (fail closed).
pub const NO_EDITS_REASON: &str = "echo_mixed_no_edits";

/// What one comparison checks.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Target {
    /// A mixed child's edit: its generated text against the parent span it
    /// replaces.
    Span,
    /// A mixed child's insertion (`s == e`): its generated text against the
    /// empty span.
    Insertion,
    /// A mirror: the whole child against its whole parent.
    Mirror,
}

/// One check's result.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    /// Not an echo.
    Distinct,
    /// The same bytes.
    ByteIdentical,
    /// Different bytes, the same whitespace-folded text.
    WhitespaceFolded,
    /// The recorded span is not a character-boundary range of the parent, so
    /// the child cannot be compared; it fails closed.
    SpanOutsideParent,
}

/// One comparison's result: both checks on the same pair of spans.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct Echo {
    pub target: Target,
    /// Check (a): the raw response span against the raw source span.
    pub raw: Outcome,
    /// Check (b): both spans after `render`.
    pub rendered: Outcome,
}

fn raw_reason(target: Target, outcome: Outcome) -> Option<&'static str> {
    Some(match (target, outcome) {
        (_, Outcome::Distinct) => return None,
        (_, Outcome::SpanOutsideParent) => "echo_span_outside_parent",
        (Target::Span, Outcome::ByteIdentical) => "echo_span_byte_identical",
        (Target::Span, Outcome::WhitespaceFolded) => "echo_span_whitespace_folded_identical",
        (Target::Insertion, Outcome::ByteIdentical) => "echo_insertion_byte_identical",
        (Target::Insertion, Outcome::WhitespaceFolded) => "echo_insertion_whitespace_folded_identical",
        (Target::Mirror, Outcome::ByteIdentical) => "echo_mirror_byte_identical",
        (Target::Mirror, Outcome::WhitespaceFolded) => "echo_mirror_whitespace_folded_identical",
    })
}

fn rendered_reason(target: Target, outcome: Outcome) -> Option<&'static str> {
    Some(match (target, outcome) {
        (_, Outcome::Distinct) => return None,
        (_, Outcome::SpanOutsideParent) => "echo_span_outside_parent",
        (Target::Span, Outcome::ByteIdentical) => "echo_span_rendered_byte_identical",
        (Target::Span, Outcome::WhitespaceFolded) => "echo_span_rendered_whitespace_folded_identical",
        (Target::Insertion, Outcome::ByteIdentical) => "echo_insertion_rendered_byte_identical",
        (Target::Insertion, Outcome::WhitespaceFolded) => "echo_insertion_rendered_whitespace_folded_identical",
        (Target::Mirror, Outcome::ByteIdentical) => "echo_mirror_rendered_byte_identical",
        (Target::Mirror, Outcome::WhitespaceFolded) => "echo_mirror_rendered_whitespace_folded_identical",
    })
}

impl Echo {
    /// The comparison echoes when either check matches.
    pub fn rejected(self) -> bool {
        self.raw != Outcome::Distinct || self.rendered != Outcome::Distinct
    }

    /// The acceptance rejection reasons, check (a)'s first; empty when the
    /// comparison passes. A span outside the parent gives one reason.
    pub fn reasons(self) -> Vec<&'static str> {
        let mut r: Vec<&'static str> = [raw_reason(self.target, self.raw), rendered_reason(self.target, self.rendered)].into_iter().flatten().collect();
        r.dedup();
        r
    }
}

/// Whether `a` and `b` are identical after whitespace folding.
pub fn folded_equal(a: &str, b: &str) -> bool {
    a.split_whitespace().eq(b.split_whitespace())
}

/// The outcome of one check: a child span against its source span.
pub fn compare(source: &str, child: &str) -> Outcome {
    if child == source {
        Outcome::ByteIdentical
    } else if folded_equal(source, child) {
        Outcome::WhitespaceFolded
    } else {
        Outcome::Distinct
    }
}

/// Check (b)'s rendering, applied alike to the source span and the response
/// span: the child normaliser's line steps (`layout::normalise_child`
/// before it rebuilds paragraphs). Each line is squashed and empty lines are
/// dropped, paired `**` and `__` emphasis is stripped, then the line's dash,
/// range and ellipsis sites and its quotes are rendered in `conv`. Lines are
/// joined by one newline. Rebuilding paragraphs moves only whitespace, which
/// the comparison folds, so a rendered response folds equal to its
/// normalised child (a test pins this).
pub fn render(t: &str, conv: &Conventions) -> String {
    let mut ch = Changes::default();
    let mut out = String::with_capacity(t.len());
    for l in layout_lines(t, &mut ch) {
        let l = strip_emphasis(&l, &mut ch);
        if l.is_empty() {
            continue;
        }
        let l = render_sites(&l, conv, &mut ch);
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(&render_quotes(&l, conv, &mut ch));
    }
    out
}

/// One comparison, both checks. `span` is the byte range of `parent` that a
/// mixed child's edit replaces (`s == e` for an insertion); `None` marks a
/// mirror. `response` is the raw response span: the generated text before
/// normalisation (for a mirror, the whole raw child). `conv` is the item's
/// conventions, the ones its child is normalised to. Callers record
/// `screen_item`.
pub fn screen(parent: &str, conv: &Conventions, span: Option<(usize, usize)>, response: &str) -> Echo {
    let (target, source) = match span {
        None => (Target::Mirror, Some(parent)),
        Some((s, e)) => (if s == e { Target::Insertion } else { Target::Span }, parent.get(s..e)),
    };
    match source {
        Some(source) => Echo { target, raw: compare(source, response), rendered: compare(&render(source, conv), &render(response, conv)) },
        None => Echo { target, raw: Outcome::SpanOutsideParent, rendered: Outcome::SpanOutsideParent },
    }
}

/// One generated edit of a mixed child: the byte range `[s, e)` of the parent
/// it replaces (`s == e` for an insertion) and its raw generated text, as
/// the raw twin splices it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Edit<'a> {
    pub span: (usize, usize),
    pub raw: &'a str,
}

/// An item's child, as the screen sees it: raw, before normalisation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Child<'a> {
    /// A mirror: the whole raw child.
    Mirror(&'a str),
    /// A mixed child: its edits, in the caller's edit order.
    Mixed(&'a [Edit<'a>]),
}

/// The item verdict: one comparison per edit, in edit order (one for a
/// mirror).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Verdict {
    pub echoes: Vec<Echo>,
}

impl Verdict {
    /// The item fails when any comparison echoes (either check), or when a
    /// mixed child has no edit to compare.
    pub fn rejected(&self) -> bool {
        self.echoes.is_empty() || self.echoes.iter().any(|e| e.rejected())
    }

    /// The acceptance reasons, sorted and deduplicated (empty when accepted).
    pub fn reasons(&self) -> Vec<String> {
        if self.echoes.is_empty() {
            return vec![NO_EDITS_REASON.to_string()];
        }
        let mut r: Vec<String> = self.echoes.iter().flat_map(|e| e.reasons()).map(str::to_string).collect();
        r.sort();
        r.dedup();
        r
    }

    /// The item record: the screen id, the edit count, each comparison's
    /// target and both checks' outcomes, and the item outcome. No text.
    pub fn record(&self) -> Value {
        json!({
            "id": ECHO_SCREEN_ID,
            "edits": self.echoes.len(),
            "echoes": self.echoes,
            "rejected_edits": self.echoes.iter().filter(|e| e.rejected()).count(),
            "rejected": self.rejected(),
        })
    }
}

/// The echo screen for one item (§7.2, amendment 4 item 4.2): the verdict
/// every caller records. `conv` is the item's conventions.
pub fn screen_item(parent: &str, conv: &Conventions, child: Child) -> Verdict {
    match child {
        Child::Mirror(c) => Verdict { echoes: vec![screen(parent, conv, None, c)] },
        Child::Mixed(edits) => Verdict { echoes: edits.iter().map(|e| screen(parent, conv, Some(e.span), e.raw)).collect() },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conventions::{Choice, Dash, Ellipsis, Quote, Range};
    use crate::layout::{Structure, normalise_child};
    use crate::text;

    const PARENT: &str = "The ferry left at dawn.\n\nThe gulls  followed it out to sea\tfor a while.\n\nThen it rained.";

    fn span_of(p: &str, needle: &str) -> (usize, usize) {
        let s = p.find(needle).unwrap();
        (s, s + needle.len())
    }

    fn mixed<'a>(edits: &'a [Edit<'a>]) -> Child<'a> {
        Child::Mixed(edits)
    }

    fn parent<K>(v: K) -> Choice<K> {
        Choice { value: Some(v), source: "parent" }
    }

    /// Conventions with every class in the given style, from the parent.
    fn conv(q: Quote, dash: Dash) -> Conventions {
        Conventions { double: parent(q), single: parent(q), dash: parent(dash), range: parent(Range::En), ellipsis: parent(Ellipsis::Char) }
    }

    fn curly() -> Conventions {
        conv(Quote::Curly, Dash::EmClosed)
    }

    fn echo(target: Target, raw: Outcome, rendered: Outcome) -> Echo {
        Echo { target, raw, rendered }
    }

    use Outcome::{ByteIdentical as Byte, Distinct, WhitespaceFolded as Folded};

    #[test]
    fn a_mixed_child_echoing_its_span_fails_byte_identical_or_folded() {
        let c = curly();
        let sp = span_of(PARENT, "The gulls  followed it out to sea\tfor a while.");
        let e = screen(PARENT, &c, Some(sp), "The gulls  followed it out to sea\tfor a while.");
        assert_eq!(e, echo(Target::Span, Byte, Byte));
        assert_eq!(e.reasons(), vec!["echo_span_byte_identical", "echo_span_rendered_byte_identical"]);
        let f = screen(PARENT, &c, Some(sp), " The gulls followed it\nout to sea for\u{a0}a while.\n");
        assert_eq!(f, echo(Target::Span, Folded, Folded));
        let v = screen_item(PARENT, &c, mixed(&[Edit { span: sp, raw: "The gulls followed it out to sea for a while." }]));
        // Rendering squashes the source's run and tab, so check (b) sees the same bytes.
        assert_eq!(v.echoes, vec![echo(Target::Span, Folded, Byte)]);
        assert_eq!(v.reasons(), vec!["echo_span_rendered_byte_identical".to_string(), "echo_span_whitespace_folded_identical".to_string()]);
        assert!(v.rejected());
        // Folding is whitespace only: case and punctuation still count, raw and rendered.
        for t in ["The gulls followed it out to sea for a while!", "the gulls followed it out to sea for a while.", "The gulls followed it out to sea for a while . "] {
            assert_eq!(screen(PARENT, &c, Some(sp), t), echo(Target::Span, Distinct, Distinct), "{t:?}");
        }
        let ok = screen_item(PARENT, &c, mixed(&[Edit { span: sp, raw: "The terns followed it out to sea for a while." }]));
        assert!(!ok.rejected());
        assert!(ok.reasons().is_empty());
    }

    #[test]
    fn a_span_crossing_paragraphs_folds_its_breaks() {
        let sp = span_of(PARENT, "dawn.\n\nThe gulls");
        assert_eq!(screen(PARENT, &curly(), Some(sp), "dawn. The gulls"), echo(Target::Span, Folded, Folded));
    }

    #[test]
    fn a_mirror_echoing_its_parent_fails() {
        let c = curly();
        assert_eq!(
            screen_item(PARENT, &c, Child::Mirror(PARENT)).reasons(),
            vec!["echo_mirror_byte_identical".to_string(), "echo_mirror_rendered_byte_identical".to_string()]
        );
        let folded = text::normalized(PARENT);
        let v = screen_item(PARENT, &c, Child::Mirror(&folded));
        assert_eq!(v.echoes, vec![echo(Target::Mirror, Folded, Folded)]);
        assert_eq!(v.reasons(), vec!["echo_mirror_rendered_whitespace_folded_identical".to_string(), "echo_mirror_whitespace_folded_identical".to_string()]);
        let v = screen_item(PARENT, &c, Child::Mirror("The ferry left at dawn."));
        assert_eq!(v.echoes, vec![echo(Target::Mirror, Distinct, Distinct)]);
        assert!(!v.rejected());
    }

    #[test]
    fn folding_agrees_with_the_whitespace_normalisation() {
        let cases = ["", " ", "a", " a ", "a b", "a  b", "a\nb", "a\u{a0}b", "a\u{2003}b", "ab", "a\u{200b}b", "A b", "a\r\n\r\nb", "a\tb "];
        for x in cases {
            for y in cases {
                assert_eq!(folded_equal(x, y), text::normalized(x) == text::normalized(y), "{x:?} {y:?}");
            }
        }
    }

    #[test]
    fn a_span_outside_the_parent_fails_both_checks_closed() {
        let c = curly();
        let n = PARENT.len();
        for sp in [(0, n + 1), (5, 4), (n + 1, n + 2), (n + 1, n + 1)] {
            let e = screen(PARENT, &c, Some(sp), "anything");
            assert_eq!((e.raw, e.rendered), (Outcome::SpanOutsideParent, Outcome::SpanOutsideParent));
            assert_eq!(e.reasons(), vec!["echo_span_outside_parent"]);
        }
        // Not a character boundary.
        let p = "Caf\u{e9} au lait.";
        assert_eq!(screen(p, &c, Some((0, 4)), "Caf").raw, Outcome::SpanOutsideParent);
        assert_eq!(screen(p, &c, Some((4, 4)), "x").raw, Outcome::SpanOutsideParent);
        assert_eq!(screen(p, &c, Some((0, 5)), "Caf\u{e9}"), echo(Target::Span, Byte, Byte));
    }

    #[test]
    fn an_item_fails_when_any_edit_echoes() {
        let c = curly();
        let a = span_of(PARENT, "The ferry left at dawn.");
        let b = span_of(PARENT, "Then it rained.");
        let fresh_a = Edit { span: a, raw: "The boat sailed at first light." };
        let fresh_b = Edit { span: b, raw: "Then the rain came." };
        let echo_b = Edit { span: b, raw: "Then it  rained." };
        let ok = screen_item(PARENT, &c, mixed(&[fresh_a, fresh_b]));
        assert!(!ok.rejected());
        assert_eq!(ok.echoes.len(), 2);
        for edits in [[fresh_a, echo_b], [echo_b, fresh_a]] {
            let v = screen_item(PARENT, &c, mixed(&edits));
            assert!(v.rejected());
            assert_eq!(v.reasons(), vec!["echo_span_rendered_byte_identical".to_string(), "echo_span_whitespace_folded_identical".to_string()]);
            assert_eq!(v.echoes.iter().filter(|e| e.rejected()).count(), 1);
        }
        // Reasons are sorted and deduplicated across edits.
        let echo_a = Edit { span: a, raw: "The ferry left at dawn." };
        let v = screen_item(PARENT, &c, mixed(&[echo_b, echo_a, echo_b]));
        assert_eq!(
            v.reasons(),
            vec![
                "echo_span_byte_identical".to_string(),
                "echo_span_rendered_byte_identical".to_string(),
                "echo_span_whitespace_folded_identical".to_string()
            ]
        );
        // One edit outside the parent fails the item closed.
        let v = screen_item(PARENT, &c, mixed(&[fresh_a, Edit { span: (0, PARENT.len() + 1), raw: "x" }]));
        assert_eq!(v.reasons(), vec!["echo_span_outside_parent".to_string()]);
    }

    #[test]
    fn a_mixed_child_with_no_edit_fails_closed() {
        let v = screen_item(PARENT, &curly(), mixed(&[]));
        assert!(v.rejected());
        assert_eq!(v.reasons(), vec![NO_EDITS_REASON.to_string()]);
        assert_eq!(v.record()["edits"], json!(0));
    }

    #[test]
    fn an_insertion_echoes_only_when_it_adds_no_token() {
        let c = curly();
        let n = PARENT.len();
        let at = |t: &'static str| screen_item(PARENT, &c, mixed(&[Edit { span: (n, n), raw: t }])).echoes[0];
        assert_eq!(at(""), echo(Target::Insertion, Byte, Byte));
        assert_eq!(at("  \n"), echo(Target::Insertion, Folded, Byte));
        assert_eq!(at("More text."), echo(Target::Insertion, Distinct, Distinct));
        assert_eq!(at("").reasons(), vec!["echo_insertion_byte_identical", "echo_insertion_rendered_byte_identical"]);
        assert_eq!(at("  \n").reasons(), vec!["echo_insertion_whitespace_folded_identical", "echo_insertion_rendered_byte_identical"]);
        // Paired emphasis alone adds a raw token but no rendered one.
        assert_eq!(at("****"), echo(Target::Insertion, Distinct, Byte));
        // An insertion is never compared with neighbouring parent text.
        let s = span_of(PARENT, "Then").0;
        assert!(!screen_item(PARENT, &c, mixed(&[Edit { span: (s, s), raw: "Then it rained." }])).rejected());
        // Its counts stay apart from span echoes.
        let r = screen_item(PARENT, &c, mixed(&[Edit { span: (n, n), raw: " " }])).record();
        assert_eq!(r["echoes"], json!([{"target": "insertion", "raw": "whitespace_folded", "rendered": "byte_identical"}]));
    }

    /// The child normaliser on one verbatim copy of `parent[s..e]`.
    fn normalised_copy(parent: &str, sp: (usize, usize), conv: &Conventions) -> String {
        normalise_child(&parent[sp.0..sp.1], conv, &Structure::span(parent, sp.0, sp.1)).text
    }

    /// The B1a cases: a verbatim copy of a span holding a minority-convention
    /// glyph or paired emphasis, which normalisation changes. Each is
    /// (parent, span, a response that differs from the span only in those
    /// glyphs or that emphasis).
    const CHANGED_BY_NORMALISATION: [(&str, &str, &str); 3] = [
        (
            "\u{201C}We left early,\u{201D} she said. \u{201C}Fine,\u{201D} he said.\n\nThe clerk said \"fine\" and stamped the forms for the two of them.",
            "The clerk said \"fine\" and stamped the forms for the two of them.",
            "The clerk said \u{201C}fine\u{201D} and stamped the forms for the two of them.",
        ),
        (
            "We left early that morning and walked to the station together.\n\nIt was the **last** train of the day, and the platform was nearly empty.",
            "It was the **last** train of the day, and the platform was nearly empty.",
            "It was the last train of the day, and the platform was nearly empty.",
        ),
        (
            "The boats came in early\u{2014}nobody wanted the channel after dark\u{2014}and tied up.\n\nThe warden waited - as he always did - until the last crew was ashore.",
            "The warden waited - as he always did - until the last crew was ashore.",
            "The warden waited\u{2014}as he always did\u{2014}until the last crew was ashore.",
        ),
    ];

    #[test]
    fn a_verbatim_copy_is_caught_whatever_normalisation_does_to_it() {
        let c = curly();
        // Control: a span already in the parent's conventions. Normalisation
        // keeps the copy, and both checks catch it.
        let p0 = "\u{201C}We left early,\u{201D} she said to the porter at the gate.\n\nHe nodded and wrote the time down in the ledger before the bell.";
        let s0 = "He nodded and wrote the time down in the ledger before the bell.";
        let sp0 = span_of(p0, s0);
        assert_eq!(normalised_copy(p0, sp0, &c), s0);
        assert_eq!(screen(p0, &c, Some(sp0), s0), echo(Target::Span, Byte, Byte));

        // Normalisation changes these copies, so the normalised copy no
        // longer equals the span as it stands (the screen before amendment 4
        // passed them). Both checks now catch the verbatim copy.
        for (parent, span, _) in &CHANGED_BY_NORMALISATION {
            let sp = span_of(parent, span);
            let n = normalised_copy(parent, sp, &c);
            assert_ne!(n, *span);
            assert_eq!(compare(span, &n), Distinct, "{span:?}");
            let v = screen_item(parent, &c, mixed(&[Edit { span: sp, raw: span }]));
            assert_eq!(v.echoes, vec![echo(Target::Span, Byte, Byte)], "{span:?}");
            assert!(v.rejected());
            assert_eq!(v.reasons(), vec!["echo_span_byte_identical".to_string(), "echo_span_rendered_byte_identical".to_string()]);
            // A mirror copy of the parent is caught the same way.
            assert_eq!(screen_item(parent, &c, Child::Mirror(parent)).echoes, vec![echo(Target::Mirror, Byte, Byte)]);
        }
    }

    #[test]
    fn check_a_alone_catches_a_raw_echo_that_rendering_splits() {
        // A spaced hyphen dash renders as an em dash, but not across a line
        // break: the raw response folds equal to its span (check (a)), while
        // the rendered spans differ (check (b) passes it).
        let c = curly();
        let p = "The boats came in early and tied up.\n\nThe warden waited - as he always did - until the last crew was ashore.";
        let span = "The warden waited - as he always did - until the last crew was ashore.";
        let raw = "The warden waited -\nas he always did - until the last crew was ashore.";
        let sp = span_of(p, span);
        assert!(!folded_equal(&render(span, &c), &render(raw, &c)));
        let v = screen_item(p, &c, mixed(&[Edit { span: sp, raw }]));
        assert_eq!(v.echoes, vec![echo(Target::Span, Folded, Distinct)]);
        assert!(v.rejected());
        assert_eq!(v.reasons(), vec!["echo_span_whitespace_folded_identical".to_string()]);
    }

    #[test]
    fn check_b_alone_catches_a_response_differing_only_in_rendered_glyphs_or_emphasis() {
        let c = curly();
        for (parent, span, response) in &CHANGED_BY_NORMALISATION {
            let sp = span_of(parent, span);
            assert_eq!(compare(span, response), Distinct, "{response:?}");
            let v = screen_item(parent, &c, mixed(&[Edit { span: sp, raw: response }]));
            assert_eq!(v.echoes, vec![echo(Target::Span, Distinct, Byte)], "{response:?}");
            assert!(v.rejected());
            assert_eq!(v.reasons(), vec!["echo_span_rendered_byte_identical".to_string()]);
            // The mirror form: the whole parent with that span's glyphs or emphasis changed.
            let mirror = format!("{}{}{}", &parent[..sp.0], response, &parent[sp.1..]);
            assert_eq!(screen_item(parent, &c, Child::Mirror(&mirror)).echoes, vec![echo(Target::Mirror, Distinct, Byte)], "{response:?}");
        }
        // A word change is caught by neither check.
        let (parent, span, _) = CHANGED_BY_NORMALISATION[0];
        let sp = span_of(parent, span);
        let v = screen_item(parent, &c, mixed(&[Edit { span: sp, raw: "The clerk said \u{201C}fine\u{201D} and stamped the forms for both of them." }]));
        assert_eq!(v.echoes, vec![echo(Target::Span, Distinct, Distinct)]);
        assert!(!v.rejected());
    }

    #[test]
    fn rendering_is_the_normalisers_up_to_whitespace() {
        let ascii = conv(Quote::Ascii, Dash::HyphenSpaced);
        let texts = [
            PARENT,
            "She said \"no\" -- twice -- and left.\nIt's 1990-1995... or so.\n\n\n  **Bold** and __under__ words end here.",
            "\u{2018}Tis the \u{2018}season\u{2019}\u{2014}they said\u{2026} pages 4\u{2013}9.",
            "",
            "   \n\n",
        ];
        for t in texts {
            for c in [curly(), ascii.clone()] {
                let n = normalise_child(t, &c, &Structure::whole(t)).text;
                assert!(folded_equal(&render(t, &c), &n), "{t:?}");
            }
        }
        // Rendering is the same function on both sides: equal inputs render equal.
        assert_eq!(render(PARENT, &curly()), "The ferry left at dawn.\nThe gulls followed it out to sea for a while.\nThen it rained.");
    }

    #[test]
    fn records_hold_no_text() {
        let c = curly();
        let sp = span_of(PARENT, "Then it rained.");
        let r = screen_item(PARENT, &c, mixed(&[Edit { span: sp, raw: "Then  it rained." }])).record();
        assert_eq!(
            r,
            json!({"id": "cc_echo_screen_v1", "edits": 1, "echoes": [{"target": "span", "raw": "whitespace_folded", "rendered": "byte_identical"}], "rejected_edits": 1, "rejected": true})
        );
        let d = screen_item(PARENT, &c, mixed(&[Edit { span: sp, raw: "Then it snowed." }])).record();
        assert_eq!(d["rejected"], json!(false));
        assert_eq!(d["rejected_edits"], json!(0));
        assert!(!r.to_string().contains("rained") && !d.to_string().contains("snowed"));
        let m = screen_item(PARENT, &c, Child::Mirror(PARENT)).record();
        assert_eq!(m["echoes"], json!([{"target": "mirror", "raw": "byte_identical", "rendered": "byte_identical"}]));
    }
}
