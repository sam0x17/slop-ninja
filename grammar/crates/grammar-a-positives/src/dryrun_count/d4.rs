//! `dryrun-count-d4`: item 14.1's child-side counts 4, 5 and 6 and index
//! 60's fourth stop invariant on D4's pilot children (BUILD-PLAN 25.3,
//! "B33"; ruling W21-R3), read from recorded files only. No generation, no
//! model call, no keyed draw.
//!
//! **Children.** The finalize records (`<finalize>/train/records.jsonl`) of
//! the items that the pilot's order files list (`PILOT.json`, each order file
//! digest-checked against it): index 197's `children_rule`. Each record
//! request's record response is finalize's (the request's earliest accepted
//! attempt, as the record's `generation.requests[].attempts` give it). It is
//! read from one of two places, each by digest:
//! * a journal file that `generate` added in the pilot (the generation
//!   manifest's `added_by_generate`, index 195): that request is a **D4
//!   child**, and it is counted;
//! * the records in force: the runs copy's re-verdict journals, which its
//!   renorm summary lists with their digests (index 197's after-checks: a byte
//!   copy of B30's re-run). These are the other requests of items with
//!   several requests (index 226's reading (E)). They are read as wiring (the
//!   item check, the final document and count 6's item reading) and are not
//!   counted: they are R0's re-verdicted responses, not D4's.
//!
//! The run is refused when a record request's response is in both places or
//! in neither, when a D4 record holds no request answered in the pilot, when
//! a record outside the pilot order holds one, when an item has more than one
//! request answered in the pilot (generate judged those in sequence; this
//! wiring reads one), and when a record's cell is not its plan item's.
//!
//! **Train lines only.** Every journal and records line's split is read from
//! its bytes before any parse (`renorm::line_split`); lines of another split
//! are skipped unparsed and counted, and a line whose split cannot be read
//! refuses its file. The plan's items, requests and input are read the same
//! way (`renorm::local_batch`).
//!
//! **Wiring** (`generate --profile cc_cohort_profile_v2`'s, `FormPost`): an
//! item with one request takes `FormPost::single`'s step; a child of an item
//! with several requests was judged with every sibling accepted, which is the
//! item check at the child's own index (`item_v2` over every edit's raw text,
//! each sibling's accepted text compared, the window on the final document).
//! The verdict is `renorm::reverdict_local`'s on that step, which is
//! `generate`'s verdict with the overlap screen aside. Amendment 7's verdict
//! (count 1) goes through the same wiring.
//!
//! **Identity.** The final document assembled from the record responses'
//! laid-out texts must be the record's text (refused otherwise). Each child
//! is then counted by `Tally::count`, whose identity check re-derives its
//! laid-out text and landed verdict and compares them with `generate`'s
//! recorded ones; a mismatch is counted and stops the run.
//!
//! **The fourth invariant** (index 60's: an accepted child frame on a side
//! where the source has none) is counted from each child's profile record as
//! index 60's `figures.py` counted it (`fourth_invariant`), by cell.
use super::*;

/// The schema of a D4 counts record.
pub const D4_COUNTS_SCHEMA: &str = "slop_ninja_a_r2_d4_children_counts_v1";
/// The schema of a D4 pilot generation manifest (log index 195).
pub const D4_MANIFEST_SCHEMA: &str = "slop_ninja_a_r2_d4_pilot_generation_manifest_v1";
/// The ids-only line of a child behind the fourth invariant.
pub const FOURTH_WHAT: &str = "fourth_invariant_accepted_child_frame_where_source_has_none";
/// How the fourth invariant is read.
pub const FOURTH_RULE: &str = "index 60's fourth stop invariant (item 14.1: an accepted child frame on a side where the source has none), as index 60's figures.py counts it from a child's profile record: the record of an item with several requests is its own edit's (edits[item_check.index] when item_check.last, else edits[0]); with transfer t and source s, the source has a head (tail) frame only when t.mixed is false, s.class is not line_form and s.diag.frames.head (tail) > 0; an accepted child counts when t.head_frame_lines > 0 without a source head frame, or t.tail_frame_lines > 0 without a source tail frame; a record without a transfer is counted as such and not judged";

/// A file and the SHA-256 it must have.
#[derive(Clone, Debug)]
pub struct Bound {
    pub path: PathBuf,
    pub sha256: String,
}

/// A D4 count's recorded inputs.
#[derive(Clone, Debug)]
pub struct D4Inputs {
    /// The runs copy: the renorm summary, the re-verdict journals and the
    /// journals `generate` added.
    pub runs: PathBuf,
    /// The SHA-256 the runs copy's renorm summary must have.
    pub summary_sha256: String,
    /// The generation manifest (`D4_MANIFEST_SCHEMA`).
    pub manifest: Bound,
    /// The pilot's `PILOT.json`.
    pub pilot: Bound,
    /// The finalize records of the Train split.
    pub records: Bound,
}

/// One D4 count: the tally, the fourth invariant and what was read.
pub struct D4Counted {
    pub tally: Tally,
    pub inputs: Value,
    pub children: Value,
    pub fourth: Value,
    pub fourth_ids: Vec<Value>,
    pub fourth_gate: u64,
}

/// The items the pilot's order files list, by generator, read through
/// `PILOT.json` (refused unless its SHA-256 is the bound one; each order file
/// refused unless its SHA-256 is the one `PILOT.json` records). Returns the
/// item ids, the record and `PILOT.json`'s `plan` member.
fn pilot_order(p: &Bound) -> Result<(BTreeSet<String>, Value, Value)> {
    let v = checked_summary(&p.path, &p.sha256)?;
    let dir = p.path.parent().context("PILOT.json has no directory")?;
    let mut items = BTreeSet::new();
    let mut files = Vec::new();
    for (g, x) in v["by_generator"].as_object().context("PILOT.json by_generator")? {
        let f = &x["pilot"];
        let path = dir.join(f["path"].as_str().context("PILOT.json pilot order path")?);
        let b = fs::read(&path).with_context(|| path.display().to_string())?;
        ensure!(f["sha256"].as_str() == Some(sha(&b).as_str()), "{}: SHA-256 {} is not PILOT.json's", path.display(), sha(&b));
        let ids = crate::order::parse_order(std::str::from_utf8(&b)?).with_context(|| path.display().to_string())?;
        let n = ids.len();
        for id in ids {
            ensure!(items.insert(id), "{}: an item is in two pilot order files", path.display());
        }
        files.push(json!({"generator": g, "path": path, "sha256": sha(&b), "items": n}));
    }
    if let Some(n) = v["pilot_rows"].as_u64() {
        ensure!(items.len() as u64 == n, "the order files list {} items, PILOT.json's pilot_rows is {n}", items.len());
    }
    if let Some(n) = v["bulk_rows"].as_u64() {
        ensure!(n == 0, "PILOT.json plans {n} bulk rows: the D4 children are a pilot's");
    }
    let rec = json!({"path": p.path, "sha256": p.sha256, "order_files": files, "items": items.len()});
    Ok((items, rec, v["plan"].clone()))
}

/// Accepted pilot responses by (request id, attempt).
type Pilots = HashMap<(String, u32), Pilot>;

/// A record's requests: (request id, record attempt).
type RecRequests = Vec<(String, u32)>;

/// An accepted response of a journal file `generate` added in the pilot.
struct Pilot {
    j: Journaled,
    /// The verdict's profile record (`diag.post`).
    post: Value,
}

/// The accepted responses of the journal files `generate` added in the pilot,
/// by (request id, attempt): the manifest's `added_by_generate` journal
/// files, each refused unless its SHA-256 is the manifest's; Train lines only
/// (a line naming no split is parsed only when its `request_id`, read from its
/// bytes, is the batch's), each line's split read from its bytes first.
fn pilot_journals(runs: &Path, m: &Bound, batch: &Batch) -> Result<(Pilots, Value)> {
    let man = checked_summary(&m.path, &m.sha256)?;
    ensure!(man["schema"] == D4_MANIFEST_SCHEMA, "{}: not a D4 pilot generation manifest", m.path.display());
    let mut found = HashMap::new();
    let mut files = Vec::new();
    let mut seen: BTreeMap<String, usize> = BTreeMap::new();
    for (rel, f) in man["added_by_generate"].as_object().context("manifest added_by_generate")? {
        let name = Path::new(rel).file_name().and_then(|n| n.to_str()).unwrap_or_default();
        if !(name.starts_with("journal-") && name.ends_with(".jsonl")) {
            continue;
        }
        ensure!(name != "journal-renorm.jsonl", "the manifest lists a re-verdict journal as added by generate");
        let p = runs.join(rel);
        files.push(checked(&p, &f["sha256"])?);
        let r = BufReader::new(fs::File::open(&p)?);
        for (n, l) in r.split(b'\n').enumerate() {
            let l = l?;
            if l.iter().all(u8::is_ascii_whitespace) {
                continue;
            }
            match line_split(&l, "split") {
                LineSplit::One(s) if s == TRAIN => {}
                LineSplit::One(s) => {
                    *seen.entry(format!("lines_of_split_{s}_skipped_unparsed")).or_default() += 1;
                    continue;
                }
                LineSplit::Missing if matches!(line_split(&l, "request_id"), LineSplit::One(r) if batch.requests.contains_key(&r)) => {}
                _ => bail!("{} line {}: no split that can be read from the line's bytes; refused before any parse", p.display(), n + 1),
            }
            let v: Value = serde_json::from_slice(&l).with_context(|| format!("{} line {}", p.display(), n + 1))?;
            let rid = v["request_id"].as_str().context("request_id")?.to_string();
            if !batch.requests.contains_key(&rid) {
                *seen.entry("lines_of_other_requests".into()).or_default() += 1;
                continue;
            }
            if v["kind"] != "response" {
                *seen.entry(format!("{}_lines", v["kind"].as_str().unwrap_or("other"))).or_default() += 1;
                continue;
            }
            let vd = &v["verdict"];
            if vd["accepted"] != true {
                *seen.entry("rejected_responses".into()).or_default() += 1;
                continue;
            }
            *seen.entry("accepted_responses".into()).or_default() += 1;
            let a = v["attempt"].as_u64().context("attempt")? as u32;
            let j = Journaled {
                accepted: true,
                reasons: strs(&vd["reasons"]),
                text: vd["text"].as_str().context("a pilot verdict without text")?.to_string(),
                raw: vd["raw_text"].as_str().context("a pilot verdict without raw_text (not a profile v2 verdict)")?.to_string(),
                conv: vd["diag"]["post"]["conventions"].clone(),
                orig: json!({"diag": {"finish_reason": vd["diag"]["finish_reason"], "raw_words": vd["diag"]["raw_words"]}}),
                original_reasons: Vec::new(),
            };
            let post = vd["diag"]["post"].clone();
            ensure!(found.insert((rid, a), Pilot { j, post }).is_none(), "{}: two accepted responses of one request attempt", p.display());
        }
    }
    let rec = json!({"manifest": {"path": m.path, "sha256": m.sha256}, "files": files, "lines": seen});
    Ok((found, rec))
}

/// One finalize record, as the count reads it.
struct D4Rec {
    id: String,
    origin: String,
    domain: String,
    text: String,
    /// Each request's id and record attempt (its earliest accepted one).
    requests: RecRequests,
}

fn record_requests(v: &Value) -> Result<RecRequests> {
    let mut out = Vec::new();
    for q in v["generation"]["requests"].as_array().context("a record without generation.requests")? {
        let rid = q["request_id"].as_str().context("record request_id")?.to_string();
        let a = q["attempts"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|x| x["accepted"] == true)
            .filter_map(|x| x["attempt"].as_u64())
            .min()
            .context("a record request with no accepted attempt")? as u32;
        out.push((rid, a));
    }
    ensure!(!out.is_empty(), "a record with no request");
    Ok(out)
}

/// The finalize records (refused unless their SHA-256 is the bound one):
/// Train lines only, each line's `split` (and `frozen_split`, when it names
/// one) read from its bytes before any parse. Returns the records of the
/// pilot's items, the other records' requests and the read's record.
fn records_of(p: &Bound, items: &BTreeSet<String>) -> Result<(Vec<D4Rec>, Vec<RecRequests>, Value)> {
    let h = bound(&p.path, &p.sha256)?;
    let mut d4 = Vec::new();
    let mut others = Vec::new();
    let mut seen: BTreeMap<String, usize> = BTreeMap::new();
    let r = BufReader::new(fs::File::open(&p.path)?);
    for (n, l) in r.split(b'\n').enumerate() {
        let l = l?;
        if l.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        let split = line_split(&l, "split");
        let frozen = line_split(&l, "frozen_split");
        match (&split, &frozen) {
            (LineSplit::One(s), LineSplit::One(f)) if s == TRAIN && f == TRAIN => {}
            (LineSplit::One(s), LineSplit::Missing) if s == TRAIN => {}
            (LineSplit::One(s), LineSplit::One(f)) => {
                *seen.entry(format!("lines_of_split_{s}_frozen_{f}_skipped_unparsed")).or_default() += 1;
                continue;
            }
            (LineSplit::One(s), LineSplit::Missing) => {
                *seen.entry(format!("lines_of_split_{s}_skipped_unparsed")).or_default() += 1;
                continue;
            }
            _ => bail!("{} line {}: no split that can be read from the line's bytes; refused before any parse", p.path.display(), n + 1),
        }
        let v: Value = serde_json::from_slice(&l).with_context(|| format!("{} line {}", p.path.display(), n + 1))?;
        let id = v["id"].as_str().context("record id")?.to_string();
        let reqs = record_requests(&v)?;
        if !items.contains(&id) {
            *seen.entry("records_not_in_the_pilot_order".into()).or_default() += 1;
            others.push(reqs);
            continue;
        }
        *seen.entry("records_of_pilot_items".into()).or_default() += 1;
        ensure!(!d4.iter().any(|r: &D4Rec| r.id == id), "{} line {}: a second record of one pilot item", p.path.display(), n + 1);
        let text = v["text"].as_str().context("record text")?.to_string();
        ensure!(v["text_sha256"].as_str() == Some(sha(text.as_bytes()).as_str()), "{} line {}: the record's text is not its text_sha256", p.path.display(), n + 1);
        d4.push(D4Rec {
            id,
            origin: v["origin"].as_str().context("record origin")?.to_string(),
            domain: v["domain"].as_str().context("record domain")?.to_string(),
            text,
            requests: reqs,
        });
    }
    let rec = json!({"path": p.path, "sha256": h, "lines": seen});
    Ok((d4, others, rec))
}

/// The fourth invariant on one child's profile record (`FOURTH_RULE`):
/// `None` when the record holds no transfer, else whether it counts, with
/// whether the child has a head frame and a tail frame.
pub fn fourth_invariant(post: &Value, several: bool, accepted: bool) -> Option<(bool, bool, bool)> {
    let rec = if several {
        let eds = post["edits"].as_array().filter(|e| !e.is_empty())?;
        let ic = &post["item_check"];
        if ic["last"] == true { eds.get(ic["index"].as_u64().unwrap_or(0) as usize)? } else { &eds[0] }
    } else {
        post
    };
    let t = rec.get("transfer").filter(|t| t.as_object().is_some_and(|o| !o.is_empty()))?;
    let src = &rec["source"];
    let n = |v: &Value| v.as_f64().unwrap_or(0.0);
    let whole = t["mixed"] != true;
    let line_form = src["class"] == "line_form";
    let has_h = whole && !line_form && n(&src["diag"]["frames"]["head"]) > 0.0;
    let has_t = whole && !line_form && n(&src["diag"]["frames"]["tail"]) > 0.0;
    let (h, tl) = (n(&t["head_frame_lines"]) > 0.0, n(&t["tail_frame_lines"]) > 0.0);
    Some((accepted && ((h && !has_h) || (tl && !has_t)), h, tl))
}

/// A D4 child: its record, item and request.
struct D4Case<'a> {
    rec: &'a D4Rec,
    item: &'a renorm::RItem,
    rid: String,
}

/// The item's parts, in item order.
fn parts_of(it: &renorm::RItem, b: &Wired) -> Vec<Part> {
    match &it.spec {
        Some(spec) => spec.parts.iter().map(|(rid, s, sep)| Part { request_id: rid.clone(), span: *s, insert_sep: sep.clone() }).collect(),
        None => it.requests.iter().map(|rid| Part { request_id: rid.clone(), span: b.batch.requests[rid].source_span.unwrap_or((0, 0)), insert_sep: None }).collect(),
    }
}

/// The verdict `generate --profile` gave child `rid` under `code`: an item
/// with one request through `FormPost::single`'s step, a child of an item
/// with several through the item check at its own index with every sibling
/// accepted (see the module notes).
fn d4_verdict(code: Code, b: &Wired, it: &renorm::RItem, rid: &str, children: &HashMap<String, Child>) -> Result<CodeVerdict> {
    let parent = b.batch.parents.get(&it.parent_id).context("an item's parent is not the plan's")?.as_str();
    let r = &b.batch.requests[rid];
    let ch = &children[rid];
    let step = match &it.spec {
        None => single_step(code, None, r, parent, &ch.conv, &ch.raw),
        Some(_) => {
            let parts = parts_of(it, b);
            let idx = parts.iter().position(|p| p.request_id == rid).context("a child is not its item's")?;
            let sib: Vec<(String, String)> = parts.iter().map(|p| (children[&p.request_id].raw.clone(), children[&p.request_id].text.clone())).collect();
            item_step(code, false, &parts, idx, parent, &ch.conv, &ch.raw, Some(&sib), &[])
        }
    };
    Ok(match step {
        Ok((text, reasons)) => {
            let post = Fixed { text, reasons };
            let v = renorm::reverdict_local(&post, r, Some(parent), &ch.raw, &ch.orig, b.tokenizer.context("a local batch needs its tokenizer")?);
            Ok((v["accepted"] == true, strs(&v["reasons"]), v["text"].as_str().unwrap_or_default().to_string()))
        }
        Err(f) => Err(f),
    })
}

/// Counts D4's children of batch `b` (see the module notes): `summary` is
/// the runs copy's renorm summary, already checked against its digest.
pub(crate) fn count_d4_batch(label: &str, b: &Wired, summary: &Value, inp: &D4Inputs) -> Result<D4Counted> {
    ensure!(b.batch.kind == renorm::PlanKind::Local, "the D4 children are a local plan's");
    let (rif, rif_rec) = journal_verdicts(&inp.runs, summary, &b.batch)?;
    let (pilots, pilot_rec) = pilot_journals(&inp.runs, &inp.manifest, &b.batch)?;
    let (order, order_rec, _) = pilot_order(&inp.pilot)?;
    let (recs, others, recs_rec) = records_of(&inp.records, &order)?;
    for reqs in &others {
        ensure!(reqs.iter().all(|k| !pilots.contains_key(k)), "a record outside the pilot order holds a request answered in the pilot");
    }
    let items: HashMap<&str, &renorm::RItem> = b.batch.items.iter().map(|i| (i.item_id.as_str(), i)).collect();
    let mut children: HashMap<String, Child> = HashMap::new();
    let mut posts: HashMap<String, &Pilot> = HashMap::new();
    let mut cases: Vec<D4Case> = Vec::new();
    let mut fig: BTreeMap<String, u64> = BTreeMap::new();
    let mut bumpf = |k: &str| *fig.entry(k.to_string()).or_default() += 1;
    for rec in &recs {
        let it = *items.get(rec.id.as_str()).context("a D4 record's item is not the plan's Train item")?;
        let cell_origin = if it.kind == "mirror" { "model_only" } else { "mixed" };
        ensure!(rec.origin == cell_origin && rec.domain == it.domain, "a D4 record's cell is not its plan item's");
        let rids: Vec<&String> = rec.requests.iter().map(|x| &x.0).collect();
        ensure!(rids == it.requests.iter().collect::<Vec<_>>(), "a D4 record's requests are not its plan item's");
        let mut answered = Vec::new();
        for (rid, a) in &rec.requests {
            let key = (rid.clone(), *a);
            let (p, f) = (pilots.get(&key), rif.get(&key));
            let j = match (p, f) {
                (Some(p), None) => {
                    answered.push(rid.clone());
                    posts.insert(rid.clone(), p);
                    bumpf("record_requests_answered_in_the_pilot");
                    &p.j
                }
                (None, Some(f)) => {
                    ensure!(f.accepted, "a D4 record's response from the records in force is rejected there");
                    bumpf("record_requests_answered_in_the_records_in_force");
                    f
                }
                (Some(_), Some(_)) => bail!("a D4 record's response is in a pilot journal and in the records in force"),
                (None, None) => bail!("a D4 record's response is in no pilot journal and in no journal of the records in force (an item with several requests needs each other request's record response)"),
            };
            ensure!(j.conv.is_object(), "a record response's profile record holds no conventions");
            children.insert(rid.clone(), Child { raw: j.raw.clone(), text: j.text.clone(), conv: j.conv.clone(), orig: j.orig.clone(), original_reasons: j.original_reasons.clone() });
        }
        ensure!(!answered.is_empty(), "a D4 record holds no request answered in the pilot");
        ensure!(answered.len() == 1, "an item with several requests answered in the pilot: generate judged them in sequence, and this wiring reads one");
        bumpf(&format!("records_with_{}_requests", rec.requests.len()));
        // the final document: the record responses' laid-out texts assembled as finalize assembles them
        let parent = b.batch.parents.get(&it.parent_id).context("an item's parent is not the plan's")?;
        let reps: Vec<&str> = it.requests.iter().map(|r| children[r].text.as_str()).collect();
        let doc = if it.kind == "mirror" {
            reps[0].to_string()
        } else {
            let parts = parts_of(it, b);
            let ps: Vec<&Part> = parts.iter().collect();
            assemble(false, parent, &ps, &reps).map_err(|e| anyhow::anyhow!("a D4 record does not assemble: {e}"))?.0
        };
        ensure!(doc == rec.text, "a D4 record's text is not the assembly of its record responses");
        cases.push(D4Case { rec, item: it, rid: answered.remove(0) });
    }
    let pilot_items_without_record = order.len() - recs.len();
    let mut landed: HashMap<String, CodeVerdict> = HashMap::new();
    let mut a7: HashMap<String, CodeVerdict> = HashMap::new();
    for c in &cases {
        landed.insert(c.rid.clone(), d4_verdict(Code::Landed, b, c.item, &c.rid, &children)?);
        a7.insert(c.rid.clone(), d4_verdict(Code::A7, b, c.item, &c.rid, &children)?);
    }
    let mut tally = Tally::default();
    let mut fourth: BTreeMap<String, BTreeMap<String, u64>> = BTreeMap::new();
    let mut fourth_ids = Vec::new();
    for c in &cases {
        let rid = c.rid.as_str();
        let it = c.item;
        let parent = b.batch.parents[&it.parent_id].as_str();
        let r = &b.batch.requests[rid];
        let ch = &children[rid];
        let pilot = posts[rid];
        let parts = parts_of(it, b);
        let k = parts.iter().position(|p| p.request_id == rid).unwrap_or(0);
        // every request of a D4 record is accepted: each edit is laid out in its complete item
        let slots: Vec<Slot> = parts.iter().map(|p| Slot { span: p.span, insert_sep: None, raw: Some(children[&p.request_id].raw.as_str()) }).collect();
        let item_doc = if it.spec.is_some() { d4_item_doc(parent, &parts, &children) } else { None };
        let item = match &item_doc {
            Some((doc, edits, rids)) => rids.iter().position(|x| x == rid).map(|index| ItemDoc { parent, doc, edits, index }),
            None => None,
        };
        let src = if it.spec.is_some() {
            Source::InItem { parent, slots: &slots, index: k }
        } else {
            match r.source_span {
                None => Source::Mirror { parent },
                Some(span) => Source::Span { parent, span },
            }
        };
        let fin = if src.whole() {
            Fin::Whole
        } else {
            let reps: Vec<&str> = parts.iter().map(|p| children[&p.request_id].text.as_str()).collect();
            let ps: Vec<&Part> = parts.iter().collect();
            match assemble(false, parent, &ps, &reps) {
                Ok((_, spans)) => Fin::Doc(&c.rec.text, replacement_range(spans[k], reps[k])),
                Err(_) => Fin::Unassembled,
            }
        };
        let text_sha256 = sha(ch.text.as_bytes());
        let lv = landed.get(rid).cloned().unwrap_or(Err(Failure::Unsupported)).map(|x| (x.0, x.1));
        let av = a7.get(rid).cloned().unwrap_or(Err(Failure::Unsupported)).map(|x| (x.0, x.1));
        let before = tally.mismatched();
        tally.count(&Case {
            batch: label,
            source: "local",
            class: &c.rec.origin,
            domain: &c.rec.domain,
            item_id: &c.rec.id,
            request_id: Some(rid),
            src,
            raw: &ch.raw,
            conv: &ch.conv,
            recorded: Recorded { accepted: pilot.j.accepted, reasons: &pilot.j.reasons, text_sha256: &text_sha256 },
            landed: lv,
            a7: av,
            fin,
            item,
            remeasure_differs: count6::remeasure_differs(&pilot.post),
        });
        // the fourth invariant, on the children the tally counted
        let cell = fourth.entry(format!("local|{}|{}", c.rec.origin, c.rec.domain)).or_default();
        let mut add = |k: &str, n: u64| *cell.entry(k.to_string()).or_default() += n;
        add("children", 1);
        if tally.mismatched() > before {
            add("children_mismatched_not_judged", 1);
            continue;
        }
        match fourth_invariant(&pilot.post, it.spec.is_some(), pilot.j.accepted) {
            None => add("children_without_transfer_record", 1),
            Some((inv, h, t)) => {
                add("children_with_transfer_record", 1);
                add("children_with_head_frame", u64::from(h));
                add("children_with_tail_frame", u64::from(t));
                add("fourth_invariant", u64::from(inv));
                if inv {
                    fourth_ids.push(json!({"schema": grammar_a_dryrun::IDS_SCHEMA, "batch": label, "source": "local", "class": c.rec.origin, "domain": c.rec.domain,
                        "item_id": c.rec.id, "request_id": rid, "accepted": pilot.j.accepted, "what": FOURTH_WHAT, "detail": {"head_frame": h, "tail_frame": t}}));
                }
            }
        }
    }
    let mut ftotal: BTreeMap<String, u64> = BTreeMap::new();
    for c in fourth.values() {
        for (k, n) in c {
            *ftotal.entry(k.clone()).or_default() += n;
        }
    }
    let fourth_gate = ftotal.get("fourth_invariant").copied().unwrap_or(0);
    let without = ftotal.get("children_without_transfer_record").copied().unwrap_or(0);
    let fourth_rec = json!({"rule": FOURTH_RULE, "gate": fourth_gate, "zero": fourth_gate == 0, "children_without_transfer_record": without,
        "cells": fourth, "totals": ftotal,
        "note": "a child without a transfer record is not judged by this rule (index 60 counted such children apart); it gates nothing and is reported"});
    let children_rec = json!({"children": cases.len(), "pilot_items": order.len(), "d4_records": recs.len(), "pilot_items_without_record": pilot_items_without_record,
        "figures": fig, "pilot_journals": pilot_rec, "records_in_force_journals": rif_rec, "records": recs_rec,
        "siblings_note": "record responses from the records in force (the other requests of items with several requests) are read as wiring: the item check, the final document and count 6's item reading; they are R0's re-verdicted responses and are not counted as children"});
    let inputs = json!({"pilot": order_rec, "manifest": {"path": inp.manifest.path, "sha256": inp.manifest.sha256}, "records": {"path": inp.records.path, "sha256": inp.records.sha256},
        "records_in_force_journals": rif_rec["files"], "pilot_journals": pilot_rec["files"]});
    Ok(D4Counted { tally, inputs, children: children_rec, fourth: fourth_rec, fourth_ids, fourth_gate })
}

/// Count 6's item document of a complete D4 item: every edit laid out in
/// its item with every request's raw text, assembled; None when an edit does
/// not lay out or the item does not assemble.
fn d4_item_doc(parent: &str, parts: &[Part], children: &HashMap<String, Child>) -> Option<ItemDocOwned> {
    let slots: Vec<Slot> = parts.iter().map(|p| Slot { span: p.span, insert_sep: None, raw: children.get(&p.request_id).map(|c| c.raw.as_str()) }).collect();
    let mut laid: Vec<(String, Vec<(usize, usize)>)> = Vec::new();
    for (j, p) in parts.iter().enumerate() {
        let ch = children.get(&p.request_id)?;
        let (_, t) = engine::landed_transfer(Source::InItem { parent, slots: &slots, index: j }, &ch.conv, &ch.raw).ok()?;
        laid.push((t.text, t.paragraphs));
    }
    let reps: Vec<&str> = laid.iter().map(|x| x.0.as_str()).collect();
    let ps: Vec<&Part> = parts.iter().collect();
    let (doc, spans) = assemble(false, parent, &ps, &reps).ok()?;
    let edits = parts
        .iter()
        .zip(&laid)
        .zip(&spans)
        .map(|((p, (rep, units)), sp)| ItemEdit { span: p.span, sep: None, fin: replacement_range(*sp, rep), units: units.clone() })
        .collect();
    Some((doc, edits, parts.iter().map(|p| p.request_id.clone()).collect()))
}

/// Counts D4's children of one local plan (see the module notes): `plan` is
/// the D4 plan directory, which `PILOT.json` must name by digest; the runs
/// copy's renorm summary gives the tokenizer and the population, and its
/// prior must be the plan's.
pub fn count_d4(label: &str, plan: &Path, inp: &D4Inputs) -> Result<D4Counted> {
    let sp = inp.runs.join("RENORM-split-train.json");
    let s = checked_summary(&sp, &inp.summary_sha256)?;
    ensure!(s["schema"] == renorm::RENORM_SCHEMA && s["plan_kind"] == "local" && s["split"] == "train", "{}: not a local Train batch summary", sp.display());
    let pilot = checked_summary(&inp.pilot.path, &inp.pilot.sha256)?;
    let plan_sha = file_sha(&plan.join("plan.json"))?;
    ensure!(pilot["plan"]["plan_sha256"].as_str() == Some(plan_sha.as_str()), "the plan's plan.json is not the one PILOT.json names");
    if let Some(h) = pilot["plan"]["items_sha256"].as_str() {
        ensure!(file_sha(&plan.join("items.jsonl"))? == h, "the plan's items.jsonl is not the one PILOT.json names");
    }
    let tok_path = PathBuf::from(s["inputs"]["tokenizer"]["path"].as_str().context("inputs.tokenizer")?);
    let tok_rec = checked(&tok_path, &s["inputs"]["tokenizer"]["sha256"])?;
    let mut tok = Tokenizer::from_file(&tok_path).map_err(|e| anyhow::anyhow!(e.to_string()))?;
    tok.with_truncation(None).map_err(|e| anyhow::anyhow!(e.to_string()))?;
    tok.with_padding(None);
    let population = s["profile"]["population"].as_str().context("profile.population")?;
    ensure!(population == POPULATION, "population {population}");
    let batch = renorm::local_batch(plan, TRAIN, population, &tok)?;
    ensure!(batch.form.prior_sha256() == s["prior_sha256"].as_str().unwrap_or_default(), "the plan's prior {} is not the runs copy's {}", batch.form.prior_sha256(), s["prior_sha256"]);
    let w = Wired { batch, edits: HashMap::new(), tokenizer: Some(&tok) };
    let mut c = count_d4_batch(label, &w, &s, inp)?;
    c.inputs["summary"] = json!({"path": sp, "sha256": inp.summary_sha256});
    c.inputs["tokenizer"] = tok_rec;
    c.inputs["plan"] = w.batch.inputs.clone();
    c.inputs["plan_lines_skipped_unparsed"] = json!(w.batch.skipped);
    c.inputs["prior_sha256"] = json!(w.batch.form.prior_sha256());
    Ok(c)
}

/// Writes a D4 count's record (`D4_COUNTS_SCHEMA`) at the new path `out` and
/// its ids-only file at the new path `ids_out` (mode 600; ids, counts and
/// digests, no text). Returns the record; refuses (after writing both) when
/// any child mismatched.
pub fn write_d4(out: &Path, ids_out: &Path, label: &str, c: &D4Counted, code: &Value) -> Result<Value> {
    let mut ids = grammar_a_dryrun::ids_lines(&c.tally);
    ids.extend(c.fourth_ids.iter().cloned());
    let ih = write_new(ids_out, &jsonl(&ids)?)?;
    let mismatched = c.tally.mismatched();
    let mut rec = grammar_a_dryrun::batch_record(label, code, &c.inputs, &c.children, &c.tally, json!({"path": ids_out, "sha256": ih, "lines": ids.len()}));
    rec["schema"] = json!(D4_COUNTS_SCHEMA);
    rec["fourth_invariant"] = c.fourth.clone();
    let g = &rec["counts"]["gate"];
    let gate = |k: &str| g[k]["gate"].as_u64().unwrap_or(0);
    let (c4, c5, c6) = (gate("count4_accepted_wrapped_lines_over_budget"), gate("count5_kept_unit_sharing_a_unit"), gate("count6_placed_break_paragraph_under_8_words"));
    let all = mismatched == 0 && g["all_zero"] == true && c.fourth_gate == 0;
    rec["d4_gate"] = json!({"count4": c4, "count5": c5, "count6": c6, "fourth_invariant": c.fourth_gate, "closed_split": g["count5_kept_unit_sharing_a_unit"]["closed_split"],
        "mismatched_children": mismatched, "all_gating_counts_zero": all,
        "reading": "counts 4-6 at the broadest readings of Tally's gate (W7-R1), and the fourth invariant by FOURTH_RULE; all_gating_counts_zero needs every one 0 and no mismatch"});
    let mut b = serde_json::to_vec_pretty(&rec)?;
    b.push(b'\n');
    write_new(out, &b)?;
    ensure!(mismatched == 0, "{mismatched} children mismatch their records: the run stops ({} holds the counts by reason, {} the ids)", out.display(), ids_out.display());
    Ok(rec)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::form_post::{self, FormPost, ItemSpec};
    use crate::generation::{Ctx, ReqState};
    use crate::overlap::{Overlap, PriorIndex};
    use crate::plan::{self, Item};
    use std::sync::Arc;

    const SYN: [&str; 20] = [
        "amber", "basalt", "cobalt", "delta", "ember", "fjord", "garnet", "harbor", "indigo", "juniper", "kestrel", "lantern", "meadow",
        "nectar", "orchid", "pebble", "quartz", "river", "saffron", "timber",
    ];
    const ALT: [&str; 20] = [
        "acorn", "bramble", "canyon", "dune", "estuary", "fern", "glacier", "heather", "inlet", "jasper", "knoll", "lichen", "marsh",
        "nettle", "oasis", "prairie", "quarry", "ridge", "sorrel", "thicket",
    ];
    const LIST: &str = "- Acorn bramble canyon dune estuary fern.\n- Glacier heather inlet jasper knoll lichen.";

    fn prose(vocab: &[&str], seed: u64, n: usize, len: usize) -> String {
        let mut st = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        let mut out: Vec<String> = Vec::new();
        for _ in 0..n {
            let ws: Vec<&str> = (0..len)
                .map(|_| {
                    st = st.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                    vocab[((st >> 33) as usize) % vocab.len()]
                })
                .collect();
            let s = ws.join(" ");
            out.push(format!("{}{}.", s[..1].to_uppercase(), &s[1..]));
        }
        out.join(" ")
    }

    /// P1: three paragraphs, about 200 words; P2: one paragraph, about 160.
    fn parents() -> HashMap<String, String> {
        let a = format!("{} The keeper called it \u{201C}amber harbor\u{201D} again. {}", prose(&SYN, 1, 3, 11), prose(&SYN, 2, 2, 10));
        let b = prose(&SYN, 3, 6, 11);
        let c = format!("{} They named the shore \u{201C}quiet fjord\u{201D} later. {}", prose(&SYN, 4, 2, 12), prose(&SYN, 5, 3, 11));
        [("p1".to_string(), format!("{a}\n\n{b}\n\n{c}")), ("p2".to_string(), prose(&SYN, 7, 14, 12))].into()
    }

    fn domain(p: &str) -> &'static str {
        if p == "p1" { "fiction" } else { "news" }
    }

    fn span_of(p: &str, i: usize) -> (usize, usize) {
        let s = text::sentences(p)[i];
        (s.start, s.end)
    }

    fn tokenizer() -> Tokenizer {
        r#"{"version":"1.0","truncation":null,"padding":null,"added_tokens":[],"normalizer":null,"pre_tokenizer":{"type":"Whitespace"},"post_processor":null,"decoder":null,"model":{"type":"WordLevel","vocab":{"[UNK]":0},"unk_token":"[UNK]"}}"#
            .parse()
            .unwrap()
    }

    fn request(id: &str, item: &str, parent: &str, span: Option<(usize, usize)>, target: usize) -> Request {
        Request {
            request_id: id.into(),
            item_id: item.into(),
            parent_id: parent.into(),
            split: "train".into(),
            generator: "g".into(),
            endpoint: "chat".into(),
            template_id: "t".into(),
            prompt: format!("prompt {id}"),
            span_index: 0,
            source_span: span,
            target_words: target,
            cut_budget: None,
            decoding: plan::profile("A", 0.7, 0.9, 1536),
            request_sha256: sha(id.as_bytes()),
            seed: 7,
            op: None,
            cohort: None,
            attempt_limit: None,
        }
    }

    fn chat_body(content: &str) -> Value {
        json!({"choices":[{"message":{"content":content},"finish_reason":"stop"}],"usage":{},"timings":{}})
    }

    fn ctx(post: Option<Arc<dyn PostProcess>>) -> Ctx {
        Ctx {
            endpoint: String::new(),
            alias: "m".into(),
            config: json!({}),
            tokenizer: tokenizer(),
            overlap: Overlap::new(PriorIndex::empty()),
            parents: parents(),
            client: generation::client().unwrap(),
            post,
            doc_words: None,
        }
    }

    fn lines(r: &Request, a: u32, parsed: &Value, v: Value) -> Vec<Value> {
        vec![
            json!({"kind":"intent","request_id":r.request_id,"item_id":r.item_id,"split":r.split,"generator":r.generator,"attempt":a,"seed":r.seed + a,"t":1,"body":{"prompt":r.prompt}}),
            json!({"kind":"response","request_id":r.request_id,"item_id":r.item_id,"split":r.split,"generator":r.generator,"attempt":a,"seed":r.seed + a,
                "t_start":1,"t_end":2,"elapsed_ms":1,"status":200,"raw_body":serde_json::to_string(parsed).unwrap(),"timings":{},"usage":{},"verdict":v}),
        ]
    }

    /// One plan item and its requests: `spans` empty for a mirror.
    struct Spec {
        id: &'static str,
        parent: &'static str,
        spans: Vec<(usize, usize)>,
        /// Each request's R0 reply (attempt 0) and pilot reply (attempt 1,
        /// None when R0's response is the record response).
        replies: Vec<(String, Option<String>)>,
        /// Listed in the pilot order.
        d4: bool,
    }

    fn specs(ps: &HashMap<String, String>) -> Vec<Spec> {
        let (q1, q2) = (&ps["p1"], &ps["p2"]);
        let sw = |q: &str, sp: (usize, usize)| text::words(&q[sp.0..sp.1]);
        let (s9, s1, s12, s3, s14) = (span_of(q1, 9), span_of(q1, 1), span_of(q1, 12), span_of(q1, 3), span_of(q1, 14));
        let z = span_of(q2, 4);
        let f = span_of(q2, 8);
        vec![
            // a mirror rejected at R0, accepted in the pilot
            Spec { id: "i-m", parent: "p1", spans: vec![], replies: vec![(String::new(), Some(prose(&ALT, 11, 18, 11)))], d4: true },
            // a span: a list (accepted with the profile off, rejected by the re-verdict), then prose in the pilot
            Spec { id: "i-s", parent: "p1", spans: vec![s9], replies: vec![(LIST.into(), Some(prose(&ALT, 13, 1, sw(q1, s9))))], d4: true },
            // two spans: the first accepted at R0, the last regenerated (the item check at index 1)
            Spec { id: "i-x", parent: "p1", spans: vec![s1, s12], replies: vec![(prose(&ALT, 31, 1, sw(q1, s1)), None), (LIST.into(), Some(prose(&ALT, 32, 1, sw(q1, s12))))], d4: true },
            // two spans: the last accepted at R0, the first regenerated (the item check at index 0)
            Spec { id: "i-y", parent: "p1", spans: vec![s3, s14], replies: vec![(LIST.into(), Some(prose(&ALT, 41, 1, sw(q1, s3)))), (prose(&ALT, 42, 1, sw(q1, s14)), None)], d4: true },
            // an R0 record (not a D4 item)
            Spec { id: "i-z", parent: "p2", spans: vec![z], replies: vec![(prose(&ALT, 51, 1, sw(q2, z)), None)], d4: false },
            // a D4 item the pilot rejects again: no record
            Spec { id: "i-f", parent: "p2", spans: vec![f], replies: vec![(LIST.into(), Some(LIST.into()))], d4: true },
        ]
    }

    fn rid(s: &Spec, k: usize) -> String {
        format!("{}-{k}", s.id)
    }

    fn requests_of(ps: &HashMap<String, String>, s: &Spec) -> Vec<Request> {
        let q = &ps[s.parent];
        if s.spans.is_empty() {
            return vec![request(&rid(s, 0), s.id, s.parent, None, text::words(q))];
        }
        s.spans.iter().enumerate().map(|(k, sp)| request(&rid(s, k), s.id, s.parent, Some(*sp), text::words(&q[sp.0..sp.1]))).collect()
    }

    fn plan_item(ps: &HashMap<String, String>, s: &Spec, reqs: &[Request]) -> Item {
        Item {
            item_id: s.id.into(),
            split: "train".into(),
            domain: domain(s.parent).into(),
            parent_id: s.parent.into(),
            parent_text_sha256: sha(ps[s.parent].as_bytes()),
            source_group: format!("g-{}", s.parent),
            cohort: "primary".into(),
            kind: if s.spans.is_empty() { "mirror".into() } else { "span".into() },
            bin: (!s.spans.is_empty()).then(|| "R2".to_string()),
            target_bp: None,
            anchor: None,
            multi_span: s.spans.len() > 1,
            generator: "g".into(),
            decoding: plan::profile("A", 0.7, 0.9, 1536),
            template_id: "t".into(),
            source_spans: s.spans.clone(),
            requested_fraction: None,
            requests: reqs.iter().map(|r| r.request_id.clone()).collect(),
            notes: vec![],
            ranks: BTreeMap::new(),
            operation: None,
            style: None,
        }
    }

    /// The D4 fixture's files and what the count reads.
    struct Fx {
        dir: tempfile::TempDir,
        reqs: Vec<Request>,
        items: Vec<renorm::RItem>,
        inp: D4Inputs,
        summary: Value,
    }

    impl Fx {
        fn batch<'a>(&self, tok: &'a Tokenizer) -> Wired<'a> {
            let ps = parents();
            let form = FormPost::new("train2", [("p1", "fiction", ps["p1"].as_str()), ("p2", "news", ps["p2"].as_str())]).unwrap();
            let batch = Batch {
                kind: renorm::PlanKind::Local,
                name: "split-train".into(),
                form,
                requests: self.reqs.iter().map(|r| (r.request_id.clone(), r.clone())).collect(),
                items: self.items.clone(),
                parents: ps,
                tokenizer: Some(tok),
                inputs: json!({"fixture": true}),
                split: "train".into(),
                skipped: renorm::Skipped::new(),
                records: None,
            };
            Wired { batch, edits: HashMap::new(), tokenizer: Some(tok) }
        }

        fn count(&self, inp: &D4Inputs) -> Result<D4Counted> {
            let tok = tokenizer();
            let w = self.batch(&tok);
            count_d4_batch("d4-test", &w, &self.summary, inp)
        }

        fn path(&self, rel: &str) -> PathBuf {
            self.dir.path().join(rel)
        }
    }

    fn write_lines(p: &Path, lines: &[Value]) {
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, jsonl(lines).unwrap()).unwrap();
    }

    /// R0's journals (profile off), B30's re-verdict, the pilot's journals
    /// (profile v2, `generate`'s wiring, at attempt 1), finalize, the
    /// generation manifest and `PILOT.json` (see `specs`).
    fn fixture() -> Fx {
        let ps = parents();
        let all = specs(&ps);
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path().to_path_buf();
        let d = d.as_path();
        let off = ctx(None);
        let mut reqs = Vec::new();
        let mut items = Vec::new();
        let mut plan_items = Vec::new();
        let mut r0 = Vec::new();
        for s in &all {
            let rs = requests_of(&ps, s);
            for (r, (reply, _)) in rs.iter().zip(&s.replies) {
                let parsed = chat_body(reply);
                r0.extend(lines(r, 0, &parsed, generation::verdict(&off, r, 200, &parsed)));
            }
            let spec = (rs.len() > 1).then(|| ItemSpec { item_id: s.id.into(), parts: rs.iter().map(|r| (r.request_id.clone(), r.source_span.unwrap(), None)).collect() });
            items.push(renorm::RItem {
                item_id: s.id.into(),
                parent_id: s.parent.into(),
                kind: if s.spans.is_empty() { "mirror".into() } else { "span".into() },
                cohort: "primary".into(),
                domain: domain(s.parent).into(),
                requests: rs.iter().map(|r| r.request_id.clone()).collect(),
                spec,
            });
            plan_items.push(plan_item(&ps, s, &rs));
            reqs.extend(rs);
        }
        let runs0 = d.join("runs-r0");
        write_lines(&runs0.join("g/journal-1-w0.jsonl"), &r0);
        let mut fx = Fx {
            dir,
            reqs,
            items,
            inp: D4Inputs {
                runs: d.join("runs"),
                summary_sha256: String::new(),
                manifest: Bound { path: d.join("manifest.json"), sha256: String::new() },
                pilot: Bound { path: d.join("pilot/PILOT.json"), sha256: String::new() },
                records: Bound { path: d.join("finalize/train/records.jsonl"), sha256: String::new() },
            },
            summary: Value::Null,
        };
        let tok = tokenizer();
        // B30's re-verdict of R0's journals: the runs copy
        {
            let w = fx.batch(&tok);
            renorm::run_batch(&w.batch, &runs0, &fx.inp.runs).unwrap();
        }
        let sp = fx.inp.runs.join("RENORM-split-train.json");
        fx.inp.summary_sha256 = file_sha(&sp).unwrap();
        fx.summary = checked_summary(&sp, &fx.inp.summary_sha256).unwrap();
        // the pilot: generate's profile v2 wiring over the runs copy's states
        let states = generation::load_states(&[fx.inp.runs.join("g")]).unwrap();
        let item_specs: Vec<ItemSpec> = fx.items.iter().filter_map(|i| i.spec.clone()).collect();
        let fp = fx.batch(&tok).batch.form.clone().with_items(item_specs, &states).unwrap();
        let ((post, _), vf) = form_post::generate_wiring(Some(fp)).unwrap();
        let on = ctx(post);
        let mut pilot = Vec::new();
        for s in &all {
            for (k, (_, p)) in s.replies.iter().enumerate() {
                let Some(p) = p else { continue };
                let r = fx.reqs.iter().find(|r| r.request_id == rid(s, k)).unwrap();
                assert!(states.get(&r.request_id).is_none_or(|st: &ReqState| st.accepted().is_none()), "{}: accepted at R0", r.request_id);
                let parsed = chat_body(p);
                let v = vf(&on, r, 200, &parsed);
                assert_eq!(v["accepted"] == true, s.id != "i-f", "{}: {}", r.request_id, v["reasons"]);
                pilot.extend(lines(r, 1, &parsed, v));
            }
        }
        let pj = fx.inp.runs.join("g/journal-2-w0.jsonl");
        write_lines(&pj, &pilot);
        // finalize over the runs copy
        let st = generation::load_states(&[fx.inp.runs.join("g")]).unwrap();
        let requests: HashMap<String, Request> = fx.reqs.iter().map(|r| (r.request_id.clone(), r.clone())).collect();
        let humans: HashMap<String, (String, Value)> = ps.iter().map(|(k, t)| (k.clone(), (t.clone(), json!({"collection": "synthetic", "writer_key": format!("w-{k}"), "rights": "synthetic"})))).collect();
        let o = crate::finalize::finalize(&plan_items, &requests, &st, &humans, &HashMap::new(), &[], &tok, "plan").unwrap();
        let recs = o.records.get("train").cloned().unwrap_or_default();
        assert_eq!(recs.len(), 5, "{:?}", recs.iter().map(|r| r["id"].clone()).collect::<Vec<_>>());
        let mut rb = jsonl(&recs).unwrap();
        rb.extend_from_slice(NOT_TRAIN);
        fs::create_dir_all(fx.inp.records.path.parent().unwrap()).unwrap();
        fs::write(&fx.inp.records.path, &rb).unwrap();
        fx.inp.records.sha256 = sha(&rb);
        // the manifest and PILOT.json
        fx.inp.manifest.sha256 = write_manifest(&fx.inp.manifest.path, &fx.inp.runs);
        let order: String = all.iter().filter(|s| s.d4).map(|s| format!("{}\n", s.id)).collect();
        fs::create_dir_all(fx.path("pilot")).unwrap();
        fs::write(fx.path("pilot/pilot-order-g.txt"), &order).unwrap();
        let pv = json!({"schema": "test", "rows": 5, "pilot_rows": 5, "bulk_rows": 0, "by_generator": {"g": {"pilot": {"path": "pilot-order-g.txt", "sha256": sha(order.as_bytes())}}},
            "plan": {"plan_sha256": "test"}});
        let pb = serde_json::to_vec(&pv).unwrap();
        fs::write(&fx.inp.pilot.path, &pb).unwrap();
        fx.inp.pilot.sha256 = sha(&pb);
        fx
    }

    /// Lines of every split but Train that no reader may parse.
    const NOT_TRAIN: &[u8] = b"{\"split\":\"dev_select\",\"id\":\x00 never parsed\n{\"split\" : \"test\", broken\n{\"split\":\"cal\",\x00\n";

    /// The manifest of the pilot journal files under `runs` (every
    /// `journal-*.jsonl` but the re-verdict's).
    fn write_manifest(p: &Path, runs: &Path) -> String {
        let mut added = serde_json::Map::new();
        let gdir = runs.join("g");
        let mut names: Vec<String> = fs::read_dir(&gdir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().to_string()).collect();
        names.sort();
        for n in names {
            if n.starts_with("journal-") && n != "journal-renorm.jsonl" {
                let b = fs::read(gdir.join(&n)).unwrap();
                added.insert(format!("g/{n}"), json!({"bytes": b.len(), "sha256": sha(&b)}));
            }
        }
        let b = serde_json::to_vec(&json!({"schema": D4_MANIFEST_SCHEMA, "added_by_generate": added})).unwrap();
        fs::write(p, &b).unwrap();
        sha(&b)
    }

    #[test]
    fn d4_children_are_counted_from_finalize_records_pilot_journals_and_the_records_in_force() {
        engine::quiet_panics();
        let fx = fixture();
        let c = fx.count(&fx.inp).unwrap();
        assert_eq!(c.tally.mismatched(), 0, "{:?}", c.tally.mismatches);
        // children: the mirror, the span and the regenerated request of each two-span item
        assert_eq!(c.children["children"], 4, "{}", c.children);
        assert_eq!((c.children["pilot_items"].as_u64(), c.children["d4_records"].as_u64(), c.children["pilot_items_without_record"].as_u64()), (Some(5), Some(4), Some(1)));
        let f = &c.children["figures"];
        assert_eq!((f["record_requests_answered_in_the_pilot"].as_u64(), f["record_requests_answered_in_the_records_in_force"].as_u64()), (Some(4), Some(2)), "{f}");
        assert_eq!((f["records_with_1_requests"].as_u64(), f["records_with_2_requests"].as_u64()), (Some(2), Some(2)));
        assert_eq!(c.children["records"]["lines"]["records_not_in_the_pilot_order"], 1);
        // the records' non-Train lines are skipped unparsed and counted
        for k in ["lines_of_split_dev_select_skipped_unparsed", "lines_of_split_test_skipped_unparsed", "lines_of_split_cal_skipped_unparsed"] {
            assert_eq!(c.children["records"]["lines"][k], 1, "{k}: {}", c.children["records"]);
        }
        // the cells, hand-computed: the mirror is model_only, the spans mixed, all in p1's domain
        let cells = c.tally.to_json()["cells"].clone();
        assert_eq!(cells["local|model_only|fiction"]["children"], 1, "{cells}");
        assert_eq!(cells["local|mixed|fiction"]["children"], 3, "{cells}");
        assert_eq!(cells["local|mixed|fiction"]["children|mixed"], 3, "{cells}");
        assert_eq!(cells["local|model_only|fiction"]["children|mirror"], 1, "{cells}");
        assert_eq!(cells["local|mixed|fiction"]["children_accepted"], 3);
        assert!(cells.get("local|mixed|news").is_none(), "the R0 record and the failed D4 item are not children");
        // the gate on this fixture: every count 0, the fourth invariant judged on every child
        let g = c.tally.to_json()["gate"].clone();
        assert_eq!(g["all_zero"], true, "{g}");
        assert_eq!(c.fourth["gate"], 0);
        assert_eq!(c.fourth["totals"]["children_with_transfer_record"], 4, "{}", c.fourth);
        // the record
        let out = fx.path("d4.json");
        let rec = write_d4(&out, &fx.path("d4.ids.jsonl"), "d4-test", &c, &json!({"commit": "test"})).unwrap();
        assert_eq!(rec["schema"], D4_COUNTS_SCHEMA);
        assert_eq!(rec["d4_gate"]["all_gating_counts_zero"], true, "{}", rec["d4_gate"]);
        let ids = fs::read_to_string(fx.path("d4.ids.jsonl")).unwrap();
        let body = fs::read_to_string(&out).unwrap();
        for w in ["amber", "acorn", "bramble", "lichen"] {
            assert!(!ids.contains(w) && !body.contains(w), "{w}");
        }
        assert!(write_d4(&out, &fx.path("d4b.ids.jsonl"), "d4-test", &c, &json!({})).is_err(), "written once");
    }

    #[test]
    fn a_pilot_journal_of_another_digest_or_with_an_unreadable_split_is_refused_and_other_splits_are_skipped() {
        let fx = fixture();
        let pj = fx.inp.runs.join("g/journal-2-w0.jsonl");
        let orig = fs::read(&pj).unwrap();
        // a non-Train line: skipped before any parse, counted
        let mut b = orig.clone();
        b.extend_from_slice(b"{\"kind\":\"response\",\"split\":\"dev_select\",\"verdict\":\x00 never parsed\n");
        fs::write(&pj, &b).unwrap();
        let mut inp = fx.inp.clone();
        // the old manifest digest: refused
        let e = format!("{:#}", fx.count(&inp).err().unwrap());
        assert!(e.contains("is not the recorded"), "{e}");
        inp.manifest.sha256 = write_manifest(&inp.manifest.path, &inp.runs);
        let c = fx.count(&inp).unwrap();
        assert_eq!(c.children["pilot_journals"]["lines"]["lines_of_split_dev_select_skipped_unparsed"], 1, "{}", c.children["pilot_journals"]);
        assert_eq!(c.children["children"], 4);
        // a line whose split cannot be read from its bytes refuses the file
        let mut b = orig.clone();
        b.extend_from_slice(b"{\"kind\":\"response\",\"split\":7,\"verdict\":{}}\n");
        fs::write(&pj, &b).unwrap();
        inp.manifest.sha256 = write_manifest(&inp.manifest.path, &inp.runs);
        let e = format!("{:#}", fx.count(&inp).err().unwrap());
        assert!(e.contains("refused before any parse"), "{e}");
        fs::write(&pj, &orig).unwrap();
        write_manifest(&inp.manifest.path, &inp.runs);
        assert_eq!(fx.count(&fx.inp).unwrap().children["children"], 4, "the original files count again");
        // a manifest of another digest
        let mut inp2 = fx.inp.clone();
        inp2.manifest.sha256 = "0".repeat(64);
        assert!(format!("{:#}", fx.count(&inp2).err().unwrap()).contains("is not the recorded"));
        // records of another digest are refused before any line is read
        let mut inp3 = fx.inp.clone();
        inp3.records.sha256 = "0".repeat(64);
        assert!(format!("{:#}", fx.count(&inp3).err().unwrap()).contains("is not the bound"));
    }

    /// Rewrites the finalize records with `f` and returns inputs bound to them.
    fn with_records(fx: &Fx, f: impl Fn(&mut Value)) -> D4Inputs {
        let b = fs::read(&fx.inp.records.path).unwrap();
        let mut out = Vec::new();
        for l in b.split(|c| *c == b'\n').filter(|l| !l.is_empty()) {
            match serde_json::from_slice::<Value>(l) {
                Ok(mut v) => {
                    f(&mut v);
                    out.extend(serde_json::to_vec(&v).unwrap());
                }
                Err(_) => out.extend_from_slice(l),
            }
            out.push(b'\n');
        }
        let p = fx.path("finalize/train/records-edited.jsonl");
        let _ = fs::remove_file(&p);
        fs::write(&p, &out).unwrap();
        let mut inp = fx.inp.clone();
        inp.records = Bound { path: p, sha256: sha(&out) };
        inp
    }

    #[test]
    fn a_two_request_item_without_its_other_response_is_refused() {
        let fx = fixture();
        // i-x's first request's record attempt moved to one no journal holds
        let inp = with_records(&fx, |v| {
            if v["id"] == "i-x" {
                v["generation"]["requests"][0]["attempts"][0]["attempt"] = json!(5);
            }
        });
        let e = format!("{:#}", fx.count(&inp).err().unwrap());
        assert!(e.contains("in no pilot journal and in no journal of the records in force"), "{e}");
        // a record outside the pilot order holding a pilot-answered request
        let inp = with_records(&fx, |v| {
            if v["id"] == "i-s" {
                v["id"] = json!("i-s-elsewhere");
            }
        });
        let e = format!("{:#}", fx.count(&inp).err().unwrap());
        assert!(e.contains("outside the pilot order"), "{e}");
        // a record whose text is not the assembly of its responses
        let inp = with_records(&fx, |v| {
            if v["id"] == "i-s" {
                let t = format!("{} ", v["text"].as_str().unwrap());
                v["text_sha256"] = json!(sha(t.as_bytes()));
                v["text"] = json!(t);
            }
        });
        let e = format!("{:#}", fx.count(&inp).err().unwrap());
        assert!(e.contains("not the assembly"), "{e}");
        // a record whose cell is not its item's
        let inp = with_records(&fx, |v| {
            if v["id"] == "i-m" {
                v["origin"] = json!("mixed");
            }
        });
        assert!(format!("{:#}", fx.count(&inp).err().unwrap()).contains("cell"));
        // a pilot item with two records
        let inp = with_records(&fx, |v| {
            if v["id"] == "i-z" {
                v["id"] = json!("i-m");
            }
        });
        assert!(format!("{:#}", fx.count(&inp).err().unwrap()).contains("a second record of one pilot item"));
    }

    #[test]
    fn an_identity_mismatch_is_counted_and_stops_the_run() {
        engine::quiet_panics();
        let fx = fixture();
        // the pilot verdict of i-s's request records another accepted text: finalize's record follows the
        // journal, so the record assembles, and the re-derived laid-out text differs from the recorded one
        let pj = fx.inp.runs.join("g/journal-2-w0.jsonl");
        let mut out = Vec::new();
        for l in fs::read_to_string(&pj).unwrap().lines() {
            let mut v: Value = serde_json::from_str(l).unwrap();
            if v["kind"] == "response" && v["request_id"] == "i-s-0" {
                let t = v["verdict"]["text"].as_str().unwrap().replacen("acorn", "Acorn", 1).replacen(' ', "  ", 1);
                v["verdict"]["text"] = json!(t);
            }
            out.push(v);
        }
        fs::write(&pj, jsonl(&out).unwrap()).unwrap();
        let mut inp = fx.inp.clone();
        inp.manifest.sha256 = write_manifest(&inp.manifest.path, &inp.runs);
        let inp = {
            let base = with_records(&fx, |v| {
                if v["id"] == "i-s" {
                    let it = fx.items.iter().find(|i| i.item_id == "i-s").unwrap();
                    let r = fx.reqs.iter().find(|r| r.request_id == it.requests[0]).unwrap();
                    let ps = parents();
                    let t = out.iter().find(|x| x["kind"] == "response" && x["request_id"] == "i-s-0").unwrap()["verdict"]["text"].as_str().unwrap().to_string();
                    let doc = generation::assemble_laid(&ps["p1"], &[r.source_span.unwrap()], &[t.as_str()]).unwrap().0;
                    v["text_sha256"] = json!(sha(doc.as_bytes()));
                    v["text"] = json!(doc);
                }
            });
            D4Inputs { manifest: inp.manifest.clone(), ..base }
        };
        let c = fx.count(&inp).unwrap();
        assert_eq!(c.tally.mismatched(), 1, "{:?}", c.tally.mismatches);
        assert_eq!(c.fourth["totals"]["children_mismatched_not_judged"], 1);
        let e = format!("{:#}", write_d4(&fx.path("m.json"), &fx.path("m.ids.jsonl"), "d4-test", &c, &json!({})).err().unwrap());
        assert!(e.contains("mismatch"), "{e}");
        let rec: Value = serde_json::from_slice(&fs::read(fx.path("m.json")).unwrap()).unwrap();
        assert_eq!(rec["d4_gate"]["all_gating_counts_zero"], false);
    }

    #[test]
    fn a_recorded_verdict_the_wiring_does_not_rederive_is_a_mismatch() {
        engine::quiet_panics();
        let fx = fixture();
        // the item check's child (i-x's last request) and the mirror record a reason their verdict path does not give
        let pj = fx.inp.runs.join("g/journal-2-w0.jsonl");
        let mut out = Vec::new();
        for l in fs::read_to_string(&pj).unwrap().lines() {
            let mut v: Value = serde_json::from_str(l).unwrap();
            if v["kind"] == "response" && (v["request_id"] == "i-x-1" || v["request_id"] == "i-m-0") {
                v["verdict"]["reasons"] = json!(["synthetic_reason"]);
            }
            out.push(v);
        }
        fs::write(&pj, jsonl(&out).unwrap()).unwrap();
        let mut inp = fx.inp.clone();
        inp.manifest.sha256 = write_manifest(&inp.manifest.path, &inp.runs);
        let c = fx.count(&inp).unwrap();
        assert_eq!(c.tally.mismatches.get("landed_verdict_differs"), Some(&2), "{:?}", c.tally.mismatches);
        assert_eq!(c.tally.mismatched(), 2);
    }

    #[test]
    fn the_fourth_invariant_reads_the_childs_own_record_as_index_60_did() {
        let t = |m: bool, h: u64, tl: u64| json!({"mixed": m, "head_frame_lines": h, "tail_frame_lines": tl});
        let src = |h: u64, tl: u64, class: &str| json!({"class": class, "diag": {"frames": {"head": h, "tail": tl}}});
        // a whole child with a head frame where the source has one: not counted
        assert_eq!(fourth_invariant(&json!({"transfer": t(false, 1, 0), "source": src(1, 0, "prose")}), false, true), Some((false, true, false)));
        // a tail frame where the source has none: counted, accepted only
        assert_eq!(fourth_invariant(&json!({"transfer": t(false, 0, 2), "source": src(1, 0, "prose")}), false, true), Some((true, false, true)));
        assert_eq!(fourth_invariant(&json!({"transfer": t(false, 0, 2), "source": src(1, 0, "prose")}), false, false), Some((false, false, true)));
        // a line-form source has no frame; a mixed child's source frames are never its own
        assert_eq!(fourth_invariant(&json!({"transfer": t(false, 1, 0), "source": src(1, 0, "line_form")}), false, true).map(|x| x.0), Some(true));
        assert_eq!(fourth_invariant(&json!({"transfer": t(true, 1, 0), "source": src(1, 1, "prose")}), false, true).map(|x| x.0), Some(true));
        // an item's record: its own edit (the item check's index), else the first
        let item = json!({"edits": [{"transfer": t(true, 0, 0)}, {"transfer": t(true, 1, 0)}], "item_check": {"last": true, "index": 1}});
        assert_eq!(fourth_invariant(&item, true, true).map(|x| x.0), Some(true));
        let first = json!({"edits": [{"transfer": t(true, 0, 0)}, {"transfer": t(true, 1, 0)}], "item_check": {"last": false, "index": 1}});
        assert_eq!(fourth_invariant(&first, true, true).map(|x| x.0), Some(false));
        // no transfer record: not judged
        assert_eq!(fourth_invariant(&json!({"source": src(0, 0, "prose")}), false, true), None);
        assert_eq!(fourth_invariant(&json!({"transfer": {}}), false, true), None);
        assert_eq!(fourth_invariant(&json!({"edits": []}), true, true), None);
    }
}
