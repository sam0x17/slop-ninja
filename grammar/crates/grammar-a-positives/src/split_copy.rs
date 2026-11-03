//! One-split plan copies and the one-split file rule (B5b; ruling R2 of
//! BUILD-PLAN section 10.1, protocol A2 amendment 20 item 20.1, read as
//! amendment 15 item 15.4 reads a line's split).
//!
//! R0's local plans (schemas v1 and v2) keep old Dev-select requests in the
//! same per-generator files as Train, and `generate` and `finalize` parse
//! their plan's files whole. Old Dev-select is never read, scored or parsed
//! (§2.4), so D4 runs only on a **one-split copy** of such a plan, and
//! `generate` and `finalize` refuse a file that holds more than one split.
//!
//! **The copy** (`split_copy`, `slop_ninja_a_positives split-copy`):
//! * `dev_select` and `test` (either spelling) are refused as the copy's
//!   split before any file is read, and so is a spliced plan (its batches are
//!   plan populations).
//! * Every line of `items.jsonl`, `requests/<generator>.jsonl` and
//!   `plan-failures.jsonl` is selected by the split its `split` keys name,
//!   read from the line's bytes before any parse
//!   ([`crate::renorm::line_split`], the reader renorm uses; one reader, not
//!   a copy). Selected lines are copied byte for byte; lines of other splits
//!   are skipped unparsed and counted by file kind and split. A line whose
//!   split cannot be read from its bytes refuses the copy, by line number,
//!   except a plan-wide plan-failure line that names no split, item or
//!   parent (plan v2's ladder relaxation count, [`plan_wide_line`]): it is no
//!   row of any split; the copy skips it unparsed and counts it under
//!   [`PLAN_WIDE`] in its record, since it describes the source plan's
//!   ladders over every split ([`PLAN_WIDE_READING`]).
//! * Each source file is hashed first and must equal the digest the source
//!   `plan.json` records.
//! * The copy's `plan.json` is the source's with the copied files' digests
//!   and counts, only the copy's split in `inputs`, `humans` and
//!   `planned_splits`, the source's per-split tables removed, and a
//!   `split_copy` record: the source plan's digests and input names, the
//!   skipped counts and the rule.
//! * **Overlap extras** ([`OVERLAP_READING`]; protocol A2 amendment 24).
//!   When the source records a CAL input and the copy's split is not CAL,
//!   the copy keeps that input record unchanged, so `generate` screens the
//!   copy's new text against CAL humans as R0's generation did for the same
//!   plan (CAL is not in the protected index). It is no split of the copy
//!   and holds no item. A consumed-Test input is never kept: round 2 parses
//!   no consumed-Test row (amendment 20 item 20.1), and R0's local plans v1
//!   and v2 record none.
//! * The batch binding of profile v2 (`form_post::FormPost::record`) names
//!   the population, seed, prior digest and prior parents, not the plan's
//!   path or digest. The copy holds its split's items byte for byte, so
//!   renorm's output runs resume under `generate --profile --plan <copy>`.
//!
//! **The one-split rule** ([`one_split`]): before a request, items or
//! plan-failures file is parsed, every line's split keys are read from its
//! bytes; the file is refused at the first line that has no readable split,
//! names a refused split (`dev_select` or `test`, even when every line names
//! it) or names a split other than the first line's, with the line number
//! and the split's class, never an id. In a plan-failures file a plan-wide
//! line ([`plan_wide_line`]) names no split and is passed over, as the copy
//! passes over it ([`OneSplit::PlanFailures`]).
use crate::renorm::{LineSplit, line_split, refused_split};
use crate::{plan, splice, text};
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::Path;

/// The `split_copy` record a copy's `plan.json` carries.
pub const COPY_SCHEMA: &str = "slop_ninja_a_positives_split_copy_v1";

/// How a copy is made.
pub const COPY_RULE: &str = "one-split plan copy (B5b; ruling R2; protocol A2 amendment 20 item 20.1; amendment 24): every line of items.jsonl, requests/<generator>.jsonl and plan-failures.jsonl is selected by the split its split keys name, read from the line's bytes before any parse (amendment 15 item 15.4's reading, renorm::line_split); selected lines are copied byte for byte, lines of other splits skipped unparsed and counted, and a line whose split cannot be read refuses the copy by line number, except a plan-wide plan-failure line (no split, item or parent key), skipped unparsed and counted as plan_wide_without_split; dev_select and test are refused as the copy's split; plan.json keeps the copy's split input and, when the source records one and the split is not cal, the CAL input as generate's overlap extra (R0's screen for the same plan); a consumed-Test input is never kept";

/// The one-split rule of `generate`, `finalize`, `mirror-plan` and `pilot`.
pub const ONE_SPLIT_RULE: &str = "a plan file that generate, finalize, mirror-plan or pilot parses (a request file, items.jsonl, plan-failures.jsonl) holds one split: every line's split keys are read from its bytes before any parse, and the file is refused at the first line with no readable split, naming dev_select or test (refused even when every line names it), or naming another split than the first line's; in a plan-failures file a plan-wide line (no split, item or parent key) is passed over, as split-copy passes over it (ruling R2; amendment 20 item 20.1; item 15.4's reading; amendment 24)";

/// How a copy treats the plan-wide plan-failure lines.
pub const PLAN_WIDE_READING: &str = "a plan-wide plan-failure line (plan v2's ladder relaxation count: no split, item_id or parent_id key, read from its bytes) is no row of any split; split-copy skips it unparsed and counts it (plan_wide_without_split), since it describes the source plan's ladders over every split; finalize, mirror-plan and pilot pass over such a line in a plan-failures file instead of refusing it (BUILD-PLAN 10.4's 'a line whose split cannot be read refuses the copy' read for row lines)";

/// Which overlap extras a copy keeps (finding 7 of the B5b review).
pub const OVERLAP_READING: &str = "generate screens new text against the protected index, its plan's CAL and consumed-Test humans (overlap extras: neither is in the index) and every accepted generated text; R0's local plans v1 and v2 record train, dev_iter, dev_select and cal inputs and no test input, so R0's screen for them is the index plus CAL; a copy keeps the source's CAL input record unchanged when the copy's split is not cal, so D4 on the copy keeps R0's screen (protocol A2 section 2.2 lets pipeline code overlap-check); a consumed-Test input is never kept, since round 2 parses no consumed-Test row (amendment 20 item 20.1); amendment 24";

/// Plan-wide tables keyed by split that a copy drops: they describe the
/// source plan's splits, and the copy's own files are counted instead.
pub const SPLIT_TABLES: [&str; 5] = ["item_counts", "coverage", "factorial", "human_genres", "plan_failures_by_reason"];

/// The input a copy keeps beside its split's ([`OVERLAP_READING`]).
pub const OVERLAP_EXTRA: &str = "cal";

/// The class of a split name, for refusals that name no row id.
pub fn split_class(s: &str) -> &'static str {
    if refused_split(s) {
        "a refused split (old Dev-select or the consumed Test)"
    } else if s == "cal" {
        "the calibration-only split"
    } else if plan::ALL_SPLITS.contains(&s) {
        "another plan split"
    } else {
        "a name outside the plan splits"
    }
}

/// The kind of plan file [`one_split`] reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OneSplit {
    /// Items or requests: every non-blank line names the split.
    Rows,
    /// Plan failures: a plan-wide line ([`plan_wide_line`]) names no split
    /// and is passed over ([`PLAN_WIDE_READING`]).
    PlanFailures,
}

/// The one split that every line of a JSONL file names under `key`, read
/// from the lines' bytes before any line is parsed ([`ONE_SPLIT_RULE`]).
/// `None` for a file with no line that names a split. Refuses, naming `what`
/// and the line number, at the first line whose split cannot be read from
/// its bytes, that names a refused split (`dev_select`, `test`), or that
/// names another split than the first line's.
pub fn one_split(bytes: &[u8], key: &str, what: &str, kind: OneSplit) -> Result<Option<String>> {
    let mut first: Option<String> = None;
    for (n, l) in bytes.split(|&b| b == b'\n').enumerate() {
        if l.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        match line_split(l, key) {
            LineSplit::One(s) if refused_split(&s) => bail!(
                "{what} line {}: names {} ({s}): old Dev-select is never read, scored or parsed and the consumed Test is consumed (protocol A2 sections 2.3 and 2.4; amendment 20 item 20.1); refused from the line's split bytes, before any parse",
                n + 1,
                split_class(&s)
            ),
            LineSplit::One(s) => match &first {
                None => first = Some(s),
                Some(f) if *f == s => {}
                Some(f) => bail!(
                    "{what} line {}: names {} ({s}), not {f}: a plan file that generate or finalize parses holds one split (ruling R2; protocol A2 amendment 20 item 20.1); refused from the line's split bytes, before any parse; run on a one-split copy (split-copy)",
                    n + 1,
                    split_class(&s)
                ),
            },
            LineSplit::Missing if kind == OneSplit::PlanFailures && plan_wide_line(l) => {}
            _ => bail!("{what} line {}: no {key} that can be read from the line's bytes; refused before any parse", n + 1),
        }
    }
    Ok(first)
}

/// [`one_split`] on the file at `p`; returns its bytes too.
pub fn one_split_file(p: &Path, key: &str, kind: OneSplit) -> Result<(Vec<u8>, Option<String>)> {
    let b = fs::read(p).with_context(|| p.display().to_string())?;
    let s = one_split(&b, key, &p.display().to_string(), kind)?;
    Ok((b, s))
}

/// Lines skipped by a copy, by file kind and split.
pub type Skipped = BTreeMap<String, BTreeMap<String, usize>>;

/// The count key of plan-wide plan-failure lines a copy skips.
pub const PLAN_WIDE: &str = "plan_wide_without_split";

/// Whether a plan-failures line is a plan-wide record: it names no split and
/// no `item_id` or `parent_id` key, read from its bytes (plan v2's ladder
/// relaxation count is one). It is no row of any split.
pub fn plan_wide_line(l: &[u8]) -> bool {
    crate::renorm::key_values(l, "split").is_empty()
        && crate::renorm::key_values(l, "item_id").is_empty()
        && crate::renorm::key_values(l, "parent_id").is_empty()
}

/// The bytes of the lines of `bytes` whose split is `split` (with their
/// newlines), byte for byte; other lines are skipped unparsed and counted in
/// `skipped[kind]`. A line whose split cannot be read refuses, by line
/// number, except, when `plan_wide` is set (plan failures), a plan-wide line
/// ([`plan_wide_line`]), which is skipped unparsed and counted under
/// [`PLAN_WIDE`].
pub fn select_lines(bytes: &[u8], key: &str, split: &str, what: &str, kind: &str, plan_wide: bool, skipped: &mut Skipped) -> Result<(Vec<u8>, usize)> {
    let mut out = Vec::new();
    let mut n_kept = 0usize;
    for (n, l) in bytes.split(|&b| b == b'\n').enumerate() {
        if l.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        match line_split(l, key) {
            LineSplit::One(s) if s == split => {
                out.extend_from_slice(l);
                out.push(b'\n');
                n_kept += 1;
            }
            LineSplit::One(s) => *skipped.entry(kind.to_string()).or_default().entry(s).or_default() += 1,
            LineSplit::Missing if plan_wide && plan_wide_line(l) => *skipped.entry(kind.to_string()).or_default().entry(PLAN_WIDE.into()).or_default() += 1,
            _ => bail!("{what} line {}: no {key} that can be read from the line's bytes; refused before any parse, and nothing is copied", n + 1),
        }
    }
    Ok((out, n_kept))
}

fn sha(b: &[u8]) -> String {
    text::sha_hex(b)
}

/// Creates `path` with `bytes` (mode 600), refusing an existing file.
pub fn write_new(path: &Path, bytes: &[u8]) -> Result<String> {
    let mut o = fs::OpenOptions::new();
    o.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600);
    }
    let mut f = o.open(path).with_context(|| path.display().to_string())?;
    f.write_all(bytes)?;
    f.sync_all()?;
    Ok(sha(bytes))
}

/// Creates directory `d` (mode 700), refusing an existing path.
pub fn new_dir(d: &Path) -> Result<()> {
    ensure!(!d.exists(), "{} exists: the output is a new directory", d.display());
    fs::create_dir_all(d)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(d, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

/// The source plan file at `p`, hashed and checked against `want` (the
/// digest the source plan.json records).
fn checked(p: &Path, want: &Value, what: &str) -> Result<Vec<u8>> {
    let b = fs::read(p).with_context(|| p.display().to_string())?;
    ensure!(want.as_str() == Some(sha(&b).as_str()), "{what} changed since planning (its digest differs from the source plan.json's)");
    Ok(b)
}

/// Writes the one-split copy of local plan `src` for `split` to the new
/// directory `out` ([`COPY_RULE`]). Returns the copy's `split_copy` record.
pub fn split_copy(src: &Path, split: &str, out: &Path) -> Result<Value> {
    ensure!(!refused_split(split), "split {split} is refused as a copy's split: old Dev-select is never read, scored or parsed and the consumed Test is consumed (protocol A2 sections 2.3 and 2.4)");
    ensure!(plan::ALL_SPLITS.contains(&split), "split {split} is not a local plan split");
    ensure!(!out.exists(), "{} exists: the copy is a new directory", out.display());
    let pj = fs::read(src.join("plan.json")).with_context(|| src.join("plan.json").display().to_string())?;
    let meta: Value = serde_json::from_slice(&pj).context("source plan.json")?;
    ensure!(meta["schema"] != splice::PLAN_SCHEMA, "{} is a spliced plan: its batches are plan populations, and split-copy copies local plans", src.display());
    ensure!(meta.get("split_copy").is_none(), "{} is already a split copy", src.display());
    let inputs = meta["inputs"].as_object().context("source plan.json inputs")?;
    let recorded = |s: &str| inputs.get(s).is_some_and(|v| !v.is_null());
    ensure!(recorded(split), "the source plan has no {split} input");
    // the overlap extra: the source's CAL input, unchanged (OVERLAP_READING)
    let extras: Vec<&str> = if split != OVERLAP_EXTRA && recorded(OVERLAP_EXTRA) { vec![OVERLAP_EXTRA] } else { Vec::new() };
    // the source files, each hashed against its recorded digest
    let items_b = checked(&src.join("items.jsonl"), &meta["items_sha256"], "items.jsonl")?;
    let fails_b = checked(&src.join("plan-failures.jsonl"), &meta["plan_failures_sha256"], "plan-failures.jsonl")?;
    let mut req_b: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    for (g, rec) in meta["request_files"].as_object().context("source plan.json request_files")? {
        req_b.insert(g.clone(), checked(&src.join("requests").join(format!("{g}.jsonl")), &rec["sha256"], &format!("request file {g}"))?);
    }
    let mut skipped = Skipped::new();
    let (items, n_items) = select_lines(&items_b, "split", split, "items.jsonl", "items", false, &mut skipped)?;
    let (fails, n_fails) = select_lines(&fails_b, "split", split, "plan-failures.jsonl", "plan_failures", true, &mut skipped)?;
    let mut reqs: BTreeMap<String, (Vec<u8>, usize)> = BTreeMap::new();
    for (g, b) in &req_b {
        let (o, n) = select_lines(b, "split", split, &format!("requests/{g}.jsonl"), "requests", false, &mut skipped)?;
        if n > 0 {
            reqs.insert(g.clone(), (o, n));
        }
    }
    ensure!(n_items > 0, "the source plan has no {split} item");
    new_dir(out)?;
    new_dir(&out.join("requests"))?;
    let items_sha = write_new(&out.join("items.jsonl"), &items)?;
    let fails_sha = write_new(&out.join("plan-failures.jsonl"), &fails)?;
    let mut req_files = serde_json::Map::new();
    let mut n_reqs = 0usize;
    for (g, (b, n)) in &reqs {
        let h = write_new(&out.join("requests").join(format!("{g}.jsonl")), b)?;
        req_files.insert(g.clone(), json!({"requests": n, "sha256": h}));
        n_reqs += n;
    }
    let mut m = meta.as_object().cloned().context("source plan.json")?;
    for k in SPLIT_TABLES {
        m.remove(k);
    }
    let mut keep_inputs = serde_json::Map::new();
    keep_inputs.insert(split.into(), inputs[split].clone());
    for x in &extras {
        keep_inputs.insert((*x).into(), inputs[*x].clone());
    }
    let source_inputs: Vec<&String> = inputs.iter().filter(|(_, v)| !v.is_null()).map(|(k, _)| k).collect();
    let not_kept: Vec<&String> = source_inputs.iter().copied().filter(|k| !keep_inputs.contains_key(k.as_str())).collect();
    m.insert("inputs".into(), Value::Object(keep_inputs));
    if let Some(h) = meta["humans"].as_object() {
        let mut hk = serde_json::Map::new();
        for s in std::iter::once(split).chain(extras.iter().copied()) {
            if let Some(v) = h.get(s) {
                hk.insert(s.into(), v.clone());
            }
        }
        m.insert("humans".into(), Value::Object(hk));
    }
    if m.contains_key("planned_splits") {
        m.insert("planned_splits".into(), json!([split]));
    }
    m.insert("items".into(), json!(n_items));
    m.insert("items_sha256".into(), json!(items_sha));
    m.insert("requests".into(), json!(n_reqs));
    m.insert("request_files".into(), Value::Object(req_files));
    m.insert("plan_failures".into(), json!(n_fails));
    m.insert("plan_failures_sha256".into(), json!(fails_sha));
    let source_reqs: BTreeMap<&String, &Value> = meta["request_files"].as_object().unwrap().iter().map(|(g, r)| (g, &r["sha256"])).collect();
    let rec = json!({"schema": COPY_SCHEMA, "code": crate::CODE_ID, "rule": COPY_RULE, "split": split, "overlap_extras": extras,
        "overlap_reading": OVERLAP_READING, "plan_wide_reading": PLAN_WIDE_READING,
        "source": {"plan": src, "plan_sha256": sha(&pj), "items_sha256": meta["items_sha256"], "plan_failures_sha256": meta["plan_failures_sha256"],
            "request_files_sha256": source_reqs, "schema": meta["schema"], "inputs": source_inputs, "inputs_not_kept": not_kept},
        "copied_lines": {"items": n_items, "plan_failures": n_fails, "requests": n_reqs},
        "lines_of_other_splits_skipped_unparsed": skipped,
        "dropped_split_tables": SPLIT_TABLES.iter().filter(|k| meta.get(**k).is_some()).collect::<Vec<_>>(),
        "binding": "profile v2's batch binding names the population, seed, prior digest and prior parents, not the plan's path or digest; the copy holds the split's items byte for byte, so a batch's runs (renorm's output among them) resume under generate --profile --plan <copy>",
        "not_replanned": "replan-check re-plans from every split's input and refuses a copy; check the source plan"});
    m.insert("split_copy".into(), rec.clone());
    let mut pb = serde_json::to_vec_pretty(&Value::Object(m))?;
    pb.push(b'\n');
    let plan_sha = write_new(&out.join("plan.json"), &pb)?;
    let mut r = rec;
    r["plan_sha256"] = json!(plan_sha);
    r["items_sha256"] = json!(items_sha);
    Ok(r)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_split_reads_lines_from_their_bytes() {
        let r = OneSplit::Rows;
        let ok = b"{\"split\":\"train\",\"a\":1}\n\n{\"x\":2, \"split\" : \"train\"}\n";
        assert_eq!(one_split(ok, "split", "f", r).unwrap().as_deref(), Some("train"));
        assert_eq!(one_split(b"\n \n", "split", "f", r).unwrap(), None);
        // a second split is refused at its line, before any parse: the line after the key is not JSON
        let two = b"{\"split\":\"train\"}\n{\"split\":\"dev_select\",\x00 never parsed\n";
        let e = one_split(two, "split", "f", r).unwrap_err().to_string();
        assert!(e.contains("line 2") && e.contains("a refused split") && !e.contains("never parsed"), "{e}");
        let cal = b"{\"split\":\"train\"}\n{\"split\":\"cal\"}\n";
        assert!(one_split(cal, "split", "f", r).unwrap_err().to_string().contains("the calibration-only split"));
        // no split key, an escape, a non-string value or two different values: unreadable
        for (bad, line) in [
            (&b"{\"split\":\"train\"}\n{\"id\":\"x\"}\n"[..], 2),
            (&b"{\"split\":\"tr\\u0061in\"}\n"[..], 1),
            (&b"{\"split\":7}\n"[..], 1),
            (&b"{\"split\":\"train\",\"o\":{\"split\":\"cal\"}}\n"[..], 1),
        ] {
            let e = one_split(bad, "split", "f", r).unwrap_err().to_string();
            assert!(e.contains(&format!("line {line}:")) && e.contains("no split that can be read"), "{e}");
        }
        // a value "split" is not a key
        assert_eq!(one_split(b"{\"kind\":\"split\",\"split\":\"train\"}\n", "split", "f", r).unwrap().as_deref(), Some("train"));
    }

    #[test]
    fn one_split_refuses_a_file_of_a_refused_split_alone_before_any_parse() {
        // B5b review finding 3: a file whose one split is dev_select or test (either spelling) is refused at
        // its first line, and the unparsable rest of the line is never read
        for (split, line) in [("dev_select", 1), ("dev-select", 1), ("test", 1)] {
            let b = format!("{{\"split\":\"{split}\",\"request_id\":\"x\",\x00 never parsed\n{{\"split\":\"{split}\", broken\n");
            for kind in [OneSplit::Rows, OneSplit::PlanFailures] {
                let e = one_split(b.as_bytes(), "split", "f", kind).unwrap_err().to_string();
                assert!(e.contains(&format!("f line {line}:")) && e.contains("a refused split") && !e.contains("never parsed"), "{e}");
            }
        }
        // after a blank line and a plan-wide plan-failure line, the first row line is the one refused
        let b = b"\n{\"stage\":\"plan\",\"reason\":\"ladder_cluster_distinctness_relaxed\",\"count\":3}\n{\"split\":\"test\",\x00\n";
        let e = one_split(b, "split", "f", OneSplit::PlanFailures).unwrap_err().to_string();
        assert!(e.contains("f line 3:") && e.contains("a refused split"), "{e}");
    }

    #[test]
    fn one_split_passes_over_a_plan_wide_plan_failure_line_only_in_plan_failures() {
        // B5b review finding 9: the copy's plan-wide reading, applied by every one-split reader
        let f = b"{\"split\":\"train\",\"item_id\":\"i\"}\n{\"stage\":\"plan\",\"reason\":\"ladder_cluster_distinctness_relaxed\",\"count\":3}\n";
        assert_eq!(one_split(f, "split", "f", OneSplit::PlanFailures).unwrap().as_deref(), Some("train"));
        let e = one_split(f, "split", "f", OneSplit::Rows).unwrap_err().to_string();
        assert!(e.contains("line 2:") && e.contains("no split that can be read"), "{e}");
        // only a plan-wide line: no split, not refused
        let only = b"{\"stage\":\"plan\",\"reason\":\"ladder_cluster_distinctness_relaxed\",\"count\":3}\n";
        assert_eq!(one_split(only, "split", "f", OneSplit::PlanFailures).unwrap(), None);
        // a row line without a split is still refused in plan failures
        let row = b"{\"split\":\"train\",\"item_id\":\"i\"}\n{\"item_id\":\"j\",\"reason\":\"r\"}\n";
        assert!(one_split(row, "split", "f", OneSplit::PlanFailures).unwrap_err().to_string().contains("line 2:"));
    }

    #[test]
    fn split_classes_name_no_id() {
        assert_eq!(split_class("dev_select"), split_class("dev-select"));
        assert_eq!(split_class("test"), "a refused split (old Dev-select or the consumed Test)");
        assert_eq!(split_class("cal"), "the calibration-only split");
        assert_eq!(split_class("dev_iter"), "another plan split");
        assert_eq!(split_class("train2"), "a name outside the plan splits");
    }

    #[test]
    fn select_lines_copies_bytes_and_counts_others() {
        let b = b"{\"split\":\"train\",\"k\":1}\n{\"split\":\"dev_select\",\x00 junk\n{ \"split\" : \"test\" broken\n{\"k\":2,\"split\":\"train\"}\n";
        let mut sk = Skipped::new();
        let (o, n) = select_lines(b, "split", "train", "f", "items", false, &mut sk).unwrap();
        assert_eq!(n, 2);
        assert_eq!(o, b"{\"split\":\"train\",\"k\":1}\n{\"k\":2,\"split\":\"train\"}\n");
        assert_eq!(sk["items"]["dev_select"], 1);
        assert_eq!(sk["items"]["test"], 1);
        let mut sk = Skipped::new();
        assert!(select_lines(b"{\"split\":\"train\"}\n{\"nosplit\":1}\n", "split", "train", "f", "items", false, &mut sk).unwrap_err().to_string().contains("line 2"));
        // plan failures: a plan-wide line (no split, item or parent key) is skipped and counted; a row line without a split is refused
        let f = b"{\"split\":\"train\",\"item_id\":\"i\"}\n{\"stage\":\"plan\",\"reason\":\"ladder_cluster_distinctness_relaxed\",\"count\":3}\n";
        let mut sk = Skipped::new();
        let (o, n) = select_lines(f, "split", "train", "f", "plan_failures", true, &mut sk).unwrap();
        assert_eq!((n, sk["plan_failures"][PLAN_WIDE]), (1, 1));
        assert_eq!(o, b"{\"split\":\"train\",\"item_id\":\"i\"}\n");
        assert!(select_lines(f, "split", "train", "f", "items", false, &mut Skipped::new()).is_err());
        let row = b"{\"item_id\":\"i\",\"reason\":\"r\"}\n";
        assert!(select_lines(row, "split", "train", "f", "plan_failures", true, &mut Skipped::new()).unwrap_err().to_string().contains("line 1"));
    }
}
