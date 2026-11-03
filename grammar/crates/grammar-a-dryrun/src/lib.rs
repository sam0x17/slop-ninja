//! `grammar-a-dryrun`: the counting kernel of the §4.3 dry run's uncounted
//! figures and child-side stop invariants (protocol A2 item 14.1; BUILD-PLAN
//! 12.3, "B23"; ruling W7-R1).
//!
//! The dry run (log index 60) re-verdicted R0's Train positives under
//! `cc_cohort_profile_v2` and recorded every child's verdict and text-free
//! profile record. Item 14.1 counts six things on those children that no
//! record holds:
//! 1. figure 8's amendment-7 comparison: verdict changes by reason,
//!    amendment 7's code (`ed25a148`, the crate `grammar-a-form-a7`, a byte
//!    copy) against the landed code (`verdict_change`);
//! 2. greeting and sign-off merges, and 3. list markers left mid-line
//!    (`figures`), under both codes;
//! 4. an accepted wrapped line over its budget that holds more than one
//!    word, a span's last line with the parent text after it included;
//! 5. a kept unit that shares a unit with other text, read on the
//!    transfer's units; 6. a body paragraph of fewer than 8 words made by a
//!    placed break (`invariants`), under the landed code.
//!
//! Counts 2-6 need the transfer's units and breaks, so every child is
//! re-derived with the landed `grammar-a-form` API from its recorded raw
//! child, source and conventions; the run refuses a child whose re-derived
//! laid-out text, or whose verdict recomputed through the same wiring,
//! differs from the recorded one (`Tally::count`; each mismatch is counted
//! and any mismatch stops the run). The verdicts stay the recorded ones:
//! this is a counting check, not a re-verdict. Conventions are taken from
//! the record (no keyed draw).
//!
//! Every function is pure apart from `engine::quiet_panics`. Outputs hold
//! counts, ids, reason codes and digests, never text.
#![forbid(unsafe_code)]

pub mod engine;
pub mod figures;
pub mod invariants;
pub mod tally;

/// The schema of one batch's counts (a partial record).
pub const BATCH_SCHEMA: &str = "slop_ninja_a_r2_reverdict_dry_run_counts_batch_v1";
/// The schema of the merged counts record (`DRY-RUN-COUNTS.json`).
pub const COUNTS_SCHEMA: &str = "slop_ninja_a_r2_reverdict_dry_run_counts_v1";
/// The schema of the ids-only file's lines.
pub const IDS_SCHEMA: &str = "slop_ninja_a_r2_reverdict_dry_run_invariant_ids_v1";
/// Amendment 7's code (log index 19; item 14.2).
pub const A7_COMMIT: &str = "ed25a1486ce57d2e015d0005291649fb3a384a2d";
/// The dry run's code (log index 60); main's form rules are its
/// (BUILD-PLAN 12 Basis), which the re-derivation identity enforces.
pub const DRY_RUN_COMMIT: &str = "8f95725b5386ccf745ed4c62f0d02467dee565cf";

/// SHA-256 hex.
pub fn sha_hex(b: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    format!("{:x}", Sha256::digest(b))
}

/// A JSON line's split as its `key` values say, read from the line's bytes
/// before any parse (amendment 15 item 15.4's reading, as
/// `grammar_a_positives::renorm::line_split`): the one value every `key`
/// names, `Missing` when none, `Unreadable` for a value that is no plain
/// string or values that differ.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Split {
    One(String),
    Missing,
    Unreadable,
}

/// The split `key` names in `line`, from its bytes.
pub fn line_split(line: &[u8], key: &str) -> Split {
    let k = format!("\"{key}\"");
    let k = k.as_bytes();
    let mut one: Option<&[u8]> = None;
    let mut any = false;
    let mut at = 0;
    while let Some(p) = line[at..].windows(k.len()).position(|w| w == k) {
        let mut i = at + p + k.len();
        at = at + p + 1;
        while i < line.len() && line[i].is_ascii_whitespace() {
            i += 1;
        }
        if line.get(i) != Some(&b':') {
            continue;
        }
        i += 1;
        while i < line.len() && line[i].is_ascii_whitespace() {
            i += 1;
        }
        any = true;
        if line.get(i) != Some(&b'"') {
            return Split::Unreadable;
        }
        let s = i + 1;
        let v = match line[s..].iter().position(|&b| b == b'"' || b == b'\\') {
            Some(e) if line[s + e] == b'"' => &line[s..s + e],
            _ => return Split::Unreadable,
        };
        match one {
            None => one = Some(v),
            Some(o) if o == v => {}
            Some(_) => return Split::Unreadable,
        }
    }
    match (any, one.map(std::str::from_utf8)) {
        (false, _) => Split::Missing,
        (true, Some(Ok(s))) => Split::One(s.to_string()),
        _ => Split::Unreadable,
    }
}

/// A request line's own split (protocol A2 amendment 32 item 32.1), read
/// from its bytes before any parse (`request_split`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OwnSplit {
    /// The line's top-level `split` value, and the value of
    /// `/voice/derived_from/split` when the line has one: the split of the
    /// human the voice was derived from, provenance only.
    One { split: String, derived: Option<String> },
    /// No top-level `split` key (and none at another path); `derived` as
    /// above.
    Missing { derived: Option<String> },
    /// A readable own split (`split`, or None when the line names none) with
    /// a `/voice/derived_from/split` that is not one plain value: repeated,
    /// or a value with an escape or that is not a string. The guard cannot
    /// read which split the voice derives from, so a line that would be a
    /// Train line refuses the plan (the B24 landing's reading of item 32.1's
    /// guard, recorded by forward amendment).
    DerivedUnreadable { split: Option<String> },
    /// Not one readable own split: a repeated top-level `split`, a
    /// top-level `split` value with an escape or that is not a string, or a
    /// `split` key at any path other than the top level and
    /// `/voice/derived_from` (a malformed line: as `line_split` reads it).
    Unreadable,
}

/// The `split` keys of a line seen so far, by path: their values (None for
/// one that is no plain string), and how many lie at any other path.
struct SplitKeys {
    top: Vec<Option<String>>,
    derived: Vec<Option<String>>,
    elsewhere: usize,
}

/// A byte-level walk of one JSON value: it reads the structure (objects,
/// arrays, strings, scalars) to find each `split` key's path and value,
/// decoding nothing else.
struct Walk<'a> {
    b: &'a [u8],
    i: usize,
}

impl Walk<'_> {
    fn ws(&mut self) {
        while self.i < self.b.len() && matches!(self.b[self.i], b' ' | b'\t' | b'\n' | b'\r') {
            self.i += 1;
        }
    }

    /// A string at `i` (its opening quote): its contents' byte range and
    /// whether it holds an escape.
    fn string(&mut self) -> Option<(usize, usize, bool)> {
        if self.b.get(self.i) != Some(&b'"') {
            return None;
        }
        let s = self.i + 1;
        let mut j = s;
        let mut esc = false;
        loop {
            match self.b.get(j)? {
                b'"' => break,
                b'\\' => {
                    esc = true;
                    j += 2;
                }
                _ => j += 1,
            }
        }
        self.i = j + 1;
        Some((s, j, esc))
    }

    /// A value at `i`, at `path` (object keys; `None` for an array element).
    fn value(&mut self, path: &mut Vec<Option<String>>, keys: &mut SplitKeys) -> Option<()> {
        self.ws();
        match *self.b.get(self.i)? {
            b'{' => {
                self.i += 1;
                self.ws();
                if self.b.get(self.i) == Some(&b'}') {
                    self.i += 1;
                    return Some(());
                }
                loop {
                    self.ws();
                    let (ks, ke, kesc) = self.string()?;
                    let key = if kesc { serde_json::from_slice::<String>(&self.b[ks - 1..ke + 1]).ok()? } else { std::str::from_utf8(&self.b[ks..ke]).ok()?.to_string() };
                    self.ws();
                    if self.b.get(self.i) != Some(&b':') {
                        return None;
                    }
                    self.i += 1;
                    self.ws();
                    if key == "split" {
                        let v = match self.b.get(self.i) {
                            Some(b'"') => {
                                let at = self.i;
                                let (vs, ve, vesc) = self.string()?;
                                self.i = at;
                                (!vesc).then(|| std::str::from_utf8(&self.b[vs..ve]).ok().map(str::to_string)).flatten()
                            }
                            _ => None,
                        };
                        let derived = path.len() == 2 && path[0].as_deref() == Some("voice") && path[1].as_deref() == Some("derived_from");
                        match (path.is_empty(), derived) {
                            (true, _) => keys.top.push(v),
                            (_, true) => keys.derived.push(v),
                            _ => keys.elsewhere += 1,
                        }
                    }
                    path.push(Some(key));
                    self.value(path, keys)?;
                    path.pop();
                    self.ws();
                    match self.b.get(self.i)? {
                        b',' => self.i += 1,
                        b'}' => {
                            self.i += 1;
                            return Some(());
                        }
                        _ => return None,
                    }
                }
            }
            b'[' => {
                self.i += 1;
                self.ws();
                if self.b.get(self.i) == Some(&b']') {
                    self.i += 1;
                    return Some(());
                }
                loop {
                    path.push(None);
                    self.value(path, keys)?;
                    path.pop();
                    self.ws();
                    match self.b.get(self.i)? {
                        b',' => self.i += 1,
                        b']' => {
                            self.i += 1;
                            return Some(());
                        }
                        _ => return None,
                    }
                }
            }
            b'"' => self.string().map(|_| ()),
            _ => {
                let s = self.i;
                while self.i < self.b.len() && !matches!(self.b[self.i], b',' | b'}' | b']' | b' ' | b'\t' | b'\n' | b'\r') {
                    self.i += 1;
                }
                (self.i > s).then_some(())
            }
        }
    }
}

/// A request line's own split (amendment 32 item 32.1), from its bytes
/// before any parse: the line's structure is walked to find every `split`
/// key's path, and no value other than a `split` value is decoded. Its own
/// split is its top-level `split` key's value. A `split` under
/// `/voice/derived_from` is the voice's provenance and is returned beside
/// it (`OwnSplit`); the caller applies the guard (§2 rules 4 and 5), and a
/// derived split that is not one plain value is `DerivedUnreadable`. Every
/// other form is `Unreadable`.
///
/// A line that is not one well-formed JSON object has no structure to
/// walk: it is read as `line_split` reads it (every `split` key's value),
/// so such a line is classified exactly as before amendment 32, and a line
/// read as Train then fails its parse.
pub fn request_split(line: &[u8]) -> OwnSplit {
    let mut w = Walk { b: line, i: 0 };
    let mut keys = SplitKeys { top: Vec::new(), derived: Vec::new(), elsewhere: 0 };
    w.ws();
    let walked = line.get(w.i) == Some(&b'{') && w.value(&mut Vec::new(), &mut keys).is_some() && {
        w.ws();
        w.i == line.len()
    };
    if !walked {
        return match line_split(line, "split") {
            Split::One(split) => OwnSplit::One { split, derived: None },
            Split::Missing => OwnSplit::Missing { derived: None },
            Split::Unreadable => OwnSplit::Unreadable,
        };
    }
    if keys.elsewhere > 0 || keys.top.len() > 1 {
        return OwnSplit::Unreadable;
    }
    let split = match keys.top.pop() {
        None => None,
        Some(None) => return OwnSplit::Unreadable,
        Some(Some(split)) => Some(split),
    };
    let derived = match (keys.derived.len(), keys.derived.pop()) {
        (_, None) => None,
        (1, Some(Some(d))) => Some(d),
        _ => return OwnSplit::DerivedUnreadable { split },
    };
    match split {
        None => OwnSplit::Missing { derived },
        Some(split) => OwnSplit::One { split, derived },
    }
}

/// The splits a Train request's voice may be derived from (amendment 32
/// item 32.1's guard; §2 rules 4 and 5: Train-mining is Train-side).
pub const TRAIN_SIDE_DERIVED: [&str; 2] = ["train", "train_mining"];

/// What every counts record states about how it counts: the definitions
/// and readings this build fixes where the text leaves them open (BUILD-PLAN
/// 12.3 open items (a)-(g)).
pub fn rules() -> serde_json::Value {
    serde_json::json!({
        "inputs": "the dry run's recorded outputs only (W7-R1; item 20.7): the batch summaries, per-request re-verdict records and output journals (the frontier: re-verdict records and the plan's accepted receipts, as the re-verdict read them), and the parents from the batches' recorded inputs, each digest-checked (R0's Train build rows and the S0+ snapshot against their bound digests); Train lines only, each line's split read from its bytes first, every other line skipped unparsed and counted (a frontier plan whose passages are not all Train, or one of R0's records whose request is no Train line, is refused); a frontier request line's split is its own top-level split key, a split under /voice/derived_from being the voice's provenance, and a derived split other than train or train_mining refuses the plan (amendment 32 item 32.1); no re-verdict re-run, generation, model call or keyed draw",
        "population": "R0's records as DRY-RUN-FIGURES.json restricts them: local items with r0_record, the S0+ snapshot's spliced items, frontier records whose request id is an R0 Train frontier row, every planned RAID, stage-3 and capture row; a child is one judged generated unit (a request of a complete R0 item whose record response the journals hold, a frontier record, a rows row with a transfer record); every cell's children are checked against DRY-RUN-FIGURES.json at the merge",
        "identity": "each child is re-derived with the landed grammar-a-form (its layout transfer from the recorded raw child, source and conventions; an edit of an item with several edits laid out in its item, in parent order, with the raw texts of the requests before it that passed, as renorm judged it: amendment 29 item 29.1) and its landed verdict recomputed through the verdict path's wiring; any difference from the recorded laid-out text, acceptance or reasons is a mismatch, and any mismatch stops the run",
        "conventions": "taken from each child's profile record (no keyed draw), the same for both codes; count 1 therefore holds no verdict change that would come from convention resolution (amendment 10 item 10.8's per-batch priors)",
        "code_swap": "amendment 7's form code (grammar-a-form at ed25a148, copied as grammar-a-form-a7) is swapped in; the verdict wiring (grammar-a-positives' FormPost steps, the final-document window, assembly, and renorm under amendment 20's readings) is the landed code's for both codes",
        "count_1": "verdict changes by reason, amendment 7's code (grammar-a-form-a7, ed25a148 byte for byte, its blob ids pinned) against the landed code, through the same wiring on the same raw child, source and conventions: accepted_to_rejected by the landed reasons, rejected_to_accepted by amendment 7's, reasons_differ by the reasons one code alone gives, a7_failed (a panic) by its code location",
        "count_2": "grammar_a_dryrun::figures: the raw child's own edge frames (layout_measure_v2's frames of the raw child); a frame line is merged unless one laid-out unit holds exactly its letters and digits and does not join parent words; under both codes, by child kind",
        "count_3": "grammar_a_dryrun::figures: a list_like raw child line (list_marker; quoted_line beside) whose marker does not stand at the start of the laid-out line holding its first letter or digit (a span's first line after the parent's text on that line); under both codes, by child kind",
        "count_4": "an accepted wrapped line over its budget with more than one word (items 10.1, 13.7): columns of lead and body over W, a mixed child's first line from the source's column (in an item, the document line's text before it, unless the replacement starts it on a new line) and its last line with the source's tail_column; two readings, on the replacement and in the final document; gate: accepted children, a line over its budget under either reading",
        "count_5": "a kept unit (a kept list line, a closed paragraph) that shares a unit of the laid-out child with other raw text, read on the transfer's units (item 13.10), on either edge of the kept unit; gate: every child; accepted beside; a closed paragraph split into several units (closed_split) is reported beside it and gates nothing",
        "count_6": "a body paragraph with a placed break on either edge (two body units of one place_breaks segment; frame edges, kept units' edges and closed paragraphs' starts and ends are fixed boundaries, amendment 30 item 30.1) and fewer than 8 words; gate: every child, the paragraph's own words; the final document's (with the parent words a span's first and last paragraphs join) beside",
        "figure_8": "counts 1-3 carry no gate of their own: they are flagged for the coordinator with the projected loss (item 10.12), with no automatic change"
    })
}

/// One batch's counts record (`BATCH_SCHEMA`): `ids` names the ids-only
/// file (path, SHA-256, lines).
pub fn batch_record(label: &str, code: &serde_json::Value, inputs: &serde_json::Value, children: &serde_json::Value, tally: &tally::Tally, ids: serde_json::Value) -> serde_json::Value {
    let mismatched = tally.mismatched();
    serde_json::json!({"schema": BATCH_SCHEMA, "batch": label, "code": code, "a7_commit": A7_COMMIT, "dry_run_commit": DRY_RUN_COMMIT,
        "model_calls": "none: the counting tool reads recorded files and runs pure form functions; it makes no model, detector or network call",
        "inputs": inputs, "children": children, "counts": tally.to_json(), "ids": ids, "rules": rules(),
        "stopped": (mismatched > 0).then(|| format!("{mismatched} children mismatch their recorded laid-out text or verdict; the run stops (BUILD-PLAN 12.3)"))})
}

/// The ids-only file's lines of a tally: the children behind each non-zero
/// stop invariant, then the mismatched children (Train records only, no
/// text).
pub fn ids_lines(tally: &tally::Tally) -> Vec<serde_json::Value> {
    tally.ids.iter().chain(&tally.mismatch_ids).cloned().collect()
}
