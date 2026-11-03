//! The two codes behind one interface: the landed `grammar-a-form` and
//! amendment 7's (`grammar-a-form-a7`, `ed25a148` byte for byte). Amendment
//! 7's form code is swapped in; the verdict wiring around it is the landed
//! code's and is shared by both codes (BUILD-PLAN 12.3 open item (b)). Every
//! call here names the code it runs, and nothing else changes.
//!
//! `git diff ed25a148 8f95725b` (amendment 7's code against the dry run's) is
//! not empty outside `grammar-a-form`:
//! * in `grammar-a-positives`, `form_post.rs` and `renorm.rs` were added and
//!   `finalize.rs`, `generation.rs`, `text.rs`, `main.rs` and
//!   `bin/spliced.rs` changed. These are the wiring: `FormPost`'s steps, the
//!   window on the final document, assembly and renorm under amendment 20's
//!   readings, which stay in force for both codes (12.3 (b));
//! * in `grammar-cc-human`, `era_norm.rs` and the gate and audit tools
//!   changed. `grammar-a-positives` does not use them.
//!
//! So the comparison is amendment 7's form rules against the landed form
//! rules inside one wiring, as item 14.2 asks. It is not a run of
//! `ed25a148`'s whole program.
//!
//! **Conventions** are the record's (`conventions` in each child's profile
//! record), read into each code's type. They are never re-resolved: a
//! re-resolution would be a keyed draw (BUILD-PLAN 12.3, Inputs), and the
//! recorded conventions are the ones the dry run's draw gave. A record whose
//! conventions do not serialise back to the same JSON is refused. One limit
//! follows: count 1 holds no verdict change that would come from resolving
//! conventions differently (amendment 10 item 10.8's per-batch priors
//! against amendment 7's resolution), because both codes judge each child
//! under the recorded conventions.
//!
//! **Panics.** Amendment 7's code panics on some lines the landed code
//! handles (`render_sites` on two spaced dashes one space apart). Every call
//! is caught (`Failure::Panic`), counted by the caller, and keeps only the
//! panic's code location: a panic message can quote the text.
use grammar_a_form as l;
use grammar_a_form_a7 as a;
use serde::de::DeserializeOwned;
use serde_json::Value;
use std::cell::RefCell;
use std::panic::{AssertUnwindSafe, catch_unwind};

/// Which code runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Code {
    /// The landed `grammar-a-form` (amendments 7, 10 and 13; the dry run's).
    Landed,
    /// Amendment 7's `grammar-a-form` at `ed25a148`.
    A7,
}

impl Code {
    pub fn id(self) -> &'static str {
        match self {
            Code::Landed => "landed",
            Code::A7 => "a7",
        }
    }
}

/// Why a child could not be run under a code.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Failure {
    /// The recorded conventions cannot be read or do not round-trip.
    Conventions(&'static str),
    /// The recorded source is not a range of its parent.
    Source,
    /// The source kind is not one this wiring judges.
    Unsupported,
    /// The code panicked; only the panic's code location is kept.
    Panic(String),
}

impl Failure {
    /// A text-free code for counts.
    pub fn code(&self) -> String {
        match self {
            Failure::Conventions(why) => format!("conventions:{why}"),
            Failure::Source => "source_outside_parent".into(),
            Failure::Unsupported => "unsupported_source".into(),
            Failure::Panic(at) => format!("panic:{}", at.rsplit(['/', '\\']).next().unwrap_or("")),
        }
    }
}

thread_local! {
    static LAST_PANIC: RefCell<Option<String>> = const { RefCell::new(None) };
    static IN_GUARD: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Installs a panic hook that never prints a panic's message (a string
/// slice's panic message quotes the string) and keeps only its code
/// location (`file:line`): silently inside a guarded call, printed to
/// stderr otherwise. The counting binaries call it before reading any
/// child.
pub fn quiet_panics() {
    std::panic::set_hook(Box::new(|info| {
        let at = info.location().map(|l| format!("{}:{}", l.file(), l.line())).unwrap_or_default();
        if IN_GUARD.with(|g| g.get()) {
            LAST_PANIC.with(|p| *p.borrow_mut() = Some(at));
        } else {
            eprintln!("panicked at {at} (message withheld: it can quote text)");
        }
    }));
}

fn guarded<T>(f: impl FnOnce() -> T) -> Result<T, Failure> {
    LAST_PANIC.with(|p| *p.borrow_mut() = None);
    let was = IN_GUARD.with(|g| g.replace(true));
    let r = catch_unwind(AssertUnwindSafe(f));
    IN_GUARD.with(|g| g.set(was));
    r.map_err(|_| Failure::Panic(LAST_PANIC.with(|p| p.borrow_mut().take()).unwrap_or_default()))
}

// ------------------------------------------------------------------ sources

/// Where a child's layout comes from, as its batch records it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source<'a> {
    /// A mirror (or a rows row laid out against its parent as a whole):
    /// `SourceLayout::whole(parent)`.
    Mirror { parent: &'a str },
    /// One replaced span `parent[span.0..span.1]`.
    Span { parent: &'a str, span: (usize, usize) },
    /// An insertion at `at` with the separator assembly puts before it.
    Insertion { parent: &'a str, at: usize, sep: &'a str },
    /// A parentless row laid out like its layout donor (§6.2).
    Donor { donor: &'a str },
    /// Edit `index` of an item with several edits, laid out in its item
    /// (`profile::lay_out_item` over `slots`: the item's every edit, with
    /// the raw texts its layout read; amendment 29 item 29.1). Amendment 7's
    /// code lays each edit out alone (`alone`).
    InItem { parent: &'a str, slots: &'a [Slot<'a>], index: usize },
}

/// One edit's slot in its item (`profile::Slot`): its splice (`s == e`
/// for an insertion), an insertion's separator and, when the layout read
/// it, the raw text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Slot<'a> {
    pub span: (usize, usize),
    pub insert_sep: Option<&'a str>,
    pub raw: Option<&'a str>,
}

fn landed_slots<'a>(slots: &[Slot<'a>]) -> Vec<l::profile::Slot<'a>> {
    slots.iter().map(|s| l::profile::Slot { span: s.span, insert_sep: s.insert_sep, raw: s.raw }).collect()
}

impl<'a> Source<'a> {
    /// The child kind (item 13.10 figure 1's kinds; a donor row is its own).
    pub fn kind(&self) -> &'static str {
        match self {
            Source::Mirror { .. } => "mirror",
            Source::Span { .. } => "mixed",
            Source::Insertion { .. } => "insertion",
            Source::Donor { .. } => "donor",
            Source::InItem { slots, index, .. } => {
                if slots[*index].span.0 == slots[*index].span.1 {
                    "insertion"
                } else {
                    "mixed"
                }
            }
        }
    }
    /// Whether the child is a whole text (a mirror or a donor row).
    pub fn whole(&self) -> bool {
        matches!(self, Source::Mirror { .. } | Source::Donor { .. })
    }
    /// The edit's source on its own: an item's edit as its span or
    /// insertion; every other source as it is.
    pub fn alone(&self) -> Source<'a> {
        match *self {
            Source::InItem { parent, slots, index } => {
                let s = slots[index];
                if s.span.0 == s.span.1 { Source::Insertion { parent, at: s.span.0, sep: s.insert_sep.unwrap_or("") } } else { Source::Span { parent, span: s.span } }
            }
            other => other,
        }
    }
}

// ------------------------------------------------------------------ conventions

/// The `source` strings `resolve` and `resolve_v2` give (both codes).
const SOURCES: [&str; 6] = ["parent", "parent_other_quote", "keyed_prior", "keyed_prior_domain", "keyed_prior_population", "none"];

fn source_str(v: &Value) -> Result<&'static str, Failure> {
    let s = v.as_str().ok_or(Failure::Conventions("source"))?;
    SOURCES.iter().find(|x| **x == s).copied().ok_or(Failure::Conventions("unknown source"))
}

fn choice<K: DeserializeOwned>(v: &Value, field: &str) -> Result<(Option<K>, &'static str), Failure> {
    let c = v.get(field).ok_or(Failure::Conventions("field missing"))?;
    let value = match c.get("value") {
        None | Some(Value::Null) => None,
        Some(x) => Some(serde_json::from_value(x.clone()).map_err(|_| Failure::Conventions("value"))?),
    };
    Ok((value, source_str(c.get("source").unwrap_or(&Value::Null))?))
}

macro_rules! conventions_of {
    ($name:ident, $m:ident) => {
        /// The recorded conventions `v` in this code's type; refused unless
        /// they serialise back to `v`.
        pub fn $name(v: &Value) -> Result<$m::conventions::Conventions, Failure> {
            use $m::conventions::{Choice, Conventions};
            let (dv, ds) = choice(v, "double")?;
            let (sv, ss) = choice(v, "single")?;
            let (av, as_) = choice(v, "dash")?;
            let (rv, rs) = choice(v, "range")?;
            let (ev, es) = choice(v, "ellipsis")?;
            let c = Conventions {
                double: Choice { value: dv, source: ds },
                single: Choice { value: sv, source: ss },
                dash: Choice { value: av, source: as_ },
                range: Choice { value: rv, source: rs },
                ellipsis: Choice { value: ev, source: es },
            };
            if serde_json::to_value(&c).ok().as_ref() != Some(v) {
                return Err(Failure::Conventions("does not round-trip"));
            }
            Ok(c)
        }
    };
}

conventions_of!(landed_conventions, l);
conventions_of!(a7_conventions, a);

// ------------------------------------------------------------------ steps

/// One child's profile step (`child_v2` or `donor_child_v2`).
#[derive(Clone, Debug, PartialEq)]
pub struct Step {
    pub text: String,
    pub reasons: Vec<String>,
    pub record: Value,
}

/// A spliced item's profile step (`item_v2`).
#[derive(Clone, Debug, PartialEq)]
pub struct ItemStep {
    pub reps: Vec<String>,
    /// The assembled document and each edit's final span.
    pub assembled: Option<(String, Vec<(usize, usize)>)>,
    pub reasons: Vec<String>,
    pub record: Value,
}

/// One edit of an item: the parent range it replaces (`s == e` for an
/// insertion), an insertion's separator and the raw text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Edit<'a> {
    pub span: (usize, usize),
    pub insert_sep: Option<&'a str>,
    pub raw: &'a str,
}

/// `profile::child_v2` under `code` (`span = None` for a mirror).
pub fn child_v2(code: Code, parent: &str, span: Option<(usize, usize)>, conv: &Value, raw: &str) -> Result<Step, Failure> {
    match code {
        Code::Landed => {
            let c = landed_conventions(conv)?;
            guarded(|| {
                let x = l::profile::child_v2(parent, span, &c, raw);
                Step { text: x.text, reasons: x.reasons, record: x.record }
            })
        }
        Code::A7 => {
            let c = a7_conventions(conv)?;
            guarded(|| {
                let x = a::profile::child_v2(parent, span, &c, raw);
                Step { text: x.text, reasons: x.reasons, record: x.record }
            })
        }
    }
}

/// `profile::donor_child_v2` under `code`, the donor's layout read by that
/// code's `SourceLayout::whole`.
pub fn donor_child_v2(code: Code, donor: &str, conv: &Value, raw: &str) -> Result<Step, Failure> {
    match code {
        Code::Landed => {
            let c = landed_conventions(conv)?;
            guarded(|| {
                let x = l::profile::donor_child_v2(&l::transfer::SourceLayout::whole(donor), &c, raw);
                Step { text: x.text, reasons: x.reasons, record: x.record }
            })
        }
        Code::A7 => {
            let c = a7_conventions(conv)?;
            guarded(|| {
                let x = a::profile::donor_child_v2(&a::transfer::SourceLayout::whole(donor), &c, raw);
                Step { text: x.text, reasons: x.reasons, record: x.record }
            })
        }
    }
}

/// `profile::item_v2` under `code`.
pub fn item_v2(code: Code, parent: &str, conv: &Value, edits: &[Edit]) -> Result<ItemStep, Failure> {
    match code {
        Code::Landed => {
            let c = landed_conventions(conv)?;
            let es: Vec<l::profile::EditV2> = edits.iter().map(|e| l::profile::EditV2 { span: e.span, insert_sep: e.insert_sep, raw: e.raw }).collect();
            guarded(|| {
                let x = l::profile::item_v2(parent, &c, &es);
                ItemStep { reps: x.reps, assembled: x.assembled.map(|a| (a.text, a.edit_spans)), reasons: x.reasons, record: x.record }
            })
        }
        Code::A7 => {
            let c = a7_conventions(conv)?;
            let es: Vec<a::profile::EditV2> = edits.iter().map(|e| a::profile::EditV2 { span: e.span, insert_sep: e.insert_sep, raw: e.raw }).collect();
            guarded(|| {
                let x = a::profile::item_v2(parent, &c, &es);
                ItemStep { reps: x.reps, assembled: x.assembled.map(|a| (a.text, a.edit_spans)), reasons: x.reasons, record: x.record }
            })
        }
    }
}

/// `profile::edit_in_item_v2` under `code`: edit `index` of an item judged
/// with its siblings pending (`FormPost::of_item`'s earlier requests). The
/// landed code lays it out in its item (`slots`, the raw texts known so far;
/// amendment 29 item 29.1); amendment 7's code is `item_v2` over the edit
/// alone.
pub fn edit_in_item_v2(code: Code, parent: &str, conv: &Value, slots: &[Slot], index: usize) -> Result<ItemStep, Failure> {
    let own = slots[index];
    match code {
        Code::Landed => {
            let c = landed_conventions(conv)?;
            let ss = landed_slots(slots);
            guarded(|| {
                let x = l::profile::edit_in_item_v2(parent, &c, &ss, index);
                ItemStep { reps: x.reps, assembled: x.assembled.map(|a| (a.text, a.edit_spans)), reasons: x.reasons, record: x.record }
            })
        }
        Code::A7 => item_v2(code, parent, conv, &[Edit { span: own.span, insert_sep: own.insert_sep, raw: own.raw.unwrap_or("") }]),
    }
}

/// `profile::splice` under `code`.
pub fn splice(code: Code, parent: &str, span: (usize, usize), rep: &str) -> Result<String, &'static str> {
    match code {
        Code::Landed => l::profile::splice(parent, span, rep),
        Code::A7 => a::profile::splice(parent, span, rep),
    }
}

/// `text::words` under `code`.
pub fn words(code: Code, s: &str) -> usize {
    match code {
        Code::Landed => l::text::words(s),
        Code::A7 => a::text::words(s),
    }
}

/// The final-document window's reasons for `n` words under `code`.
pub fn window_reasons(code: Code, n: usize) -> Vec<String> {
    match code {
        Code::Landed => l::window::reasons(n, l::window::LENGTH_WINDOW),
        Code::A7 => a::window::reasons(n, a::window::LENGTH_WINDOW),
    }
}

fn sorted(mut v: Vec<String>) -> Vec<String> {
    v.sort();
    v.dedup();
    v
}

/// A verdict: accepted, its reasons, the laid-out child and the final
/// document.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Verdict {
    pub accepted: bool,
    pub reasons: Vec<String>,
    pub text: String,
    pub doc: Option<String>,
}

/// The verdict of the frontier re-verdict (`FormCheck::check`) and of the
/// rows re-verdict (`renorm_rows`) under `code`: the child step
/// (`child_v2`, or `donor_child_v2` for a donor row), then, when the child
/// is not empty, the window on the final document (the laid-out child, or
/// the parent with the laid-out span by `profile::splice`, whose error is
/// `window_assembly_error:<e>`). Accepted when no reason holds and a final
/// document exists. An insertion has no such wiring (`Unsupported`).
pub fn simple_verdict(code: Code, src: Source, conv: &Value, raw: &str) -> Result<Verdict, Failure> {
    let step = match src {
        Source::Mirror { parent } => child_v2(code, parent, None, conv, raw)?,
        Source::Span { parent, span } => child_v2(code, parent, Some(span), conv, raw)?,
        Source::Donor { donor } => donor_child_v2(code, donor, conv, raw)?,
        Source::Insertion { .. } | Source::InItem { .. } => return Err(Failure::Unsupported),
    };
    let mut reasons = step.reasons;
    let mut doc = None;
    if !step.text.is_empty() {
        let d = match src {
            Source::Span { parent, span } => guarded(|| splice(code, parent, span, &step.text))?,
            _ => Ok(step.text.clone()),
        };
        match d {
            Ok(d) => {
                reasons.extend(window_reasons(code, words(code, &d)));
                doc = Some(d);
            }
            Err(e) => reasons.push(format!("window_assembly_error:{e}")),
        }
    }
    let reasons = sorted(reasons);
    Ok(Verdict { accepted: reasons.is_empty() && doc.is_some(), reasons, text: step.text, doc })
}

// ------------------------------------------------------------------ transfers

/// A laid-out child under one code: its text and its units' byte ranges
/// (`Transferred::paragraphs`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Laid {
    pub text: String,
    pub units: Vec<(usize, usize)>,
}

/// The landed source layout of `src` on its own (an item's edit as its
/// span or insertion: `Source::alone`).
pub fn landed_source(src: Source) -> Result<l::transfer::SourceLayout, Failure> {
    match src.alone() {
        Source::Mirror { parent } => Ok(l::transfer::SourceLayout::whole(parent)),
        Source::Donor { donor } => Ok(l::transfer::SourceLayout::whole(donor)),
        Source::Span { parent, span } => l::transfer::SourceLayout::span(parent, span.0, span.1).ok_or(Failure::Source),
        Source::Insertion { parent, at, sep } => l::transfer::SourceLayout::insertion(parent, at, sep).ok_or(Failure::Source),
        Source::InItem { .. } => Err(Failure::Unsupported),
    }
}

/// The landed transfer of `raw` from `src` (`layout_transfer_v5`), with
/// its source layout: the text `child_v2`, `item_v2`, `edit_in_item_v2` and
/// `donor_child_v2` lay out, and its diagnostics. An item's edit
/// (`Source::InItem`) is laid out in its item (`profile::lay_out_item`,
/// its own slot's raw text being `raw`), its source carrying the item's
/// columns.
pub fn landed_transfer(src: Source, conv: &Value, raw: &str) -> Result<(l::transfer::SourceLayout, l::transfer::Transferred), Failure> {
    let c = landed_conventions(conv)?;
    if let Source::InItem { parent, slots, index } = src {
        let mut ss = landed_slots(slots);
        ss[index].raw = Some(raw);
        return guarded(|| {
            let mut laid = l::profile::lay_out_item(parent, &c, &ss);
            laid[index].take().map(|x| (x.src, x.transferred)).ok_or(Failure::Source)
        })?;
    }
    guarded(|| landed_source(src).map(|s| {
        let t = l::transfer::transfer(raw, &c, &s);
        (s, t)
    }))?
}

/// The laid-out child of `raw` from `src` under `code`.
pub fn laid(code: Code, src: Source, conv: &Value, raw: &str) -> Result<Laid, Failure> {
    match code {
        Code::Landed => landed_transfer(src, conv, raw).map(|(_, t)| Laid { text: t.text, units: t.paragraphs }),
        Code::A7 => {
            let c = a7_conventions(conv)?;
            guarded(|| {
                let s = match src.alone() {
                    Source::Mirror { parent } => Some(a::transfer::SourceLayout::whole(parent)),
                    Source::Donor { donor } => Some(a::transfer::SourceLayout::whole(donor)),
                    Source::Span { parent, span } => a::transfer::SourceLayout::span(parent, span.0, span.1),
                    Source::Insertion { parent, at, sep } => a::transfer::SourceLayout::insertion(parent, at, sep),
                    Source::InItem { .. } => None,
                };
                s.map(|s| {
                    let t = a::transfer::transfer(raw, &c, &s);
                    Laid { text: t.text, units: t.paragraphs }
                })
                .ok_or(Failure::Source)
            })?
        }
    }
}
