//! The domain-supervision top-up plan from Train-mining (B5b part 2, item 2;
//! BUILD-PLAN 11.4; protocol A2 §4.3, §4.5, §6.4 rules 1–4, amendment 5
//! item 5.3, amendment 24 item 24.4's item 2).
//!
//! **S0-8.** The targets are `VALUES-R2.json`'s S0-8 (`values::supervision`:
//! the `slop_ninja_a_assemble supervision` output, read by its schema
//! string; refused while absent). Each covered domain gives the rows to add:
//! `add.humans` (rule 4), `add.local_positives` (rules 1 and 3, with
//! `add.local_for_generator_floor` per generator label) and
//! `add.frontier_records` (rule 2, waived for Enron). S0-8's domains are
//! the bundle's, which the assembler takes from the split manifests
//! (`domain_by_id`), so Train-mining humans are mapped to them by their
//! manifest's `domain`.
//!
//! **Filling** ([`TOPUP_RULE`]). Rows come from Train-mining humans and
//! generation until each rule holds or supply ends; each shortfall is
//! recorded by domain and rule.
//! * **Supply.** A Train-mining human (`frozen_split` `train_mining`, read
//!   from the line's bytes; other lines skipped unparsed and counted) is a
//!   candidate of its manifest domain when that domain is covered and not an
//!   Enron domain, its id is not an R0 Train human's, its collection has a
//!   planner domain, and its words lie in `cc_final_length_window_v1` (§6.2:
//!   the window runs at assembly on every document of every class, and a
//!   mirror is requested at no fewer than its parent's words).
//! * **Humans (rule 4).** The first `add.humans` candidates of the domain in
//!   ascending V21 key `SHA-256("slop_ninja_a_comp_r2_plan_v1:topup_human:"
//!   + id)` (ties by id) become Train-2 humans (`humans.jsonl`, written with
//!   `frozen_split` `train` and the rule).
//! * **Local positives (rules 1 and 3; D5).** `add.local_positives` new
//!   mirrors from the Train-only mirror planner (`train_mirror`; protocol A2
//!   §6.4: "top-up mirrors"; item 24.4's item 1: "D5's top-up mirrors"), one
//!   per parent: the added humans in their key order, then R0's Train humans
//!   of the domain (`--train` with its manifest, always given; in the
//!   window) in ascending V21 key `topup_parent`. Each family's quota starts
//!   at the rule-3 additions S0-8 names for its generator label (the label is
//!   the family's config `family`, as `finalize` records it). A rule-3
//!   addition of a label that is no Train instruct family's stays in the
//!   equal split of the local mirrors (S0-8 composes it into rule 1's
//!   positives), and that label's cell is recorded as a rule-3 shortfall the
//!   top-up does not fill (amendment 27).
//! * **Frontier (rule 2; Stage 0-H's top-up).** `add.frontier_records` per
//!   domain, allocated over the collector's labels by [`allocate`] and
//!   checked by [`check_allocation`] (amendment 5 item 5.3); an Enron domain
//!   gets none (§2.7). The passages are the collector's to pick.
//!
//! **Outputs** (a new directory, mode 700; files 600): `TOPUP.json`,
//! `humans.jsonl`, `parents.jsonl` (D5's Train input for
//! `train-mirror-plan --topup`) and `frontier/ALLOCATION.json`. The plan
//! is written only when the allocation passes its check; `topup-check`
//! re-checks a written plan against S0-8, and D5's step re-runs that check
//! against the register it is given (amendment 27). Every input is read
//! twice (a scan, then the selected lines): the second pass re-hashes the
//! whole file against the first pass's digest and reads each selected
//! line's split from its bytes before it parses or copies it.
use crate::split_copy::{Skipped, new_dir, write_new};
use crate::{plan, span_ops, text};
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

/// `TOPUP.json`.
pub const TOPUP_SCHEMA: &str = "slop_ninja_a_positives_topup_plan_v1";
/// `frontier/ALLOCATION.json`.
pub const ALLOCATION_SCHEMA: &str = "slop_ninja_a_positives_topup_frontier_allocation_v1";
/// `topup-check`'s record.
pub const CHECK_SCHEMA: &str = "slop_ninja_a_positives_topup_check_v1";
/// The collector's plan config whose `models[].label` are the frontier labels.
pub const FRONTIER_CONFIG_SCHEMA: &str = "slop_ninja_frontier_plan_config_v1";
/// The collector's labels of amendment 5.3's three named models (protocol A2
/// spells them gpt-6.1-terra, gpt-5.6-terra and Astra).
pub const TERRA_61: &str = "gpt-6-1-terra";
pub const TERRA_56: &str = "gpt-5-6-terra";
pub const ASTRA: &str = "gpt-6-astra";
/// V21's x of the added humans' order.
pub const KEY_HUMAN: &str = "topup_human";
/// V21's x of R0's Train parents' order.
pub const KEY_PARENT: &str = "topup_parent";
/// The Train-mining split name.
pub const TRAIN_MINING: &str = "train_mining";

/// The filling rule.
pub const TOPUP_RULE: &str = "r2_topup_plan_v1 (B5b part 2 item 2; protocol A2 sections 4.3 and 6.4, amendment 5 item 5.3, amendment 27): per S0-8 domain (VALUES-R2.json's S0-8, refused while absent), Train-mining humans (frozen_split train_mining, read from each line's bytes) of that manifest domain, not R0 Train humans (--train with its manifest, required), with a planner domain and in cc_final_length_window_v1, ordered by V21 key topup_human (ties by id; no A6 key): the first add.humans become Train-2 humans (rule 4); add.local_positives new mirrors (rules 1 and 3; the Train-only mirror planner; one per parent) take as parents the added humans in that order, then R0 Train humans of the domain in the window by V21 key topup_parent (ties by id; no A6 key); each family's quota starts at the rule-3 addition of its config family label, the rest split equally over the seven families; a rule-3 addition of a label that is no Train instruct family's stays in that equal split and its cell is recorded as a rule-3 shortfall the top-up does not fill; add.frontier_records are allocated over the collector's labels (allocation rule below) and checked; an Enron domain gets no Train-mining human and no frontier record (its humans and positives are D6's; section 2.7); every shortfall is recorded by domain and rule where supply ends";
/// The label allocation (BUILD-PLAN 11.4's open item (a): amendment 5.3
/// gives inequalities, not a split; fixed by amendment 27).
pub const ALLOCATION_RULE: &str = "r2_topup_label_allocation_v1 (amendment 5 item 5.3; section 6.4 rule 2 'label-balanced, Terra >= Astra'; section 4.5 'more to gpt-6.1-terra'; the user's 2026-10-28 decision; amendment 27): per domain the added records F are split equally over the labels of the collector config Stage 0-H's top-up collects under (every model label, as R0's frontier was label-balanced; the config bound by digest), floor(F/n) each; the remainder goes one each to gpt-6-1-terra first, then to the other labels in config order; if gpt-6-1-terra then holds no more than gpt-5-6-terra, one record moves from gpt-5-6-terra to gpt-6-1-terra; counted per domain on the added records (the next frontier wave), which implies the same inequalities over the added records in total";
/// The check.
pub const CHECK_RULE: &str = "r2_topup_label_check_v1 (amendment 5 item 5.3: the plan's allocation is checked, and no Stage 0-H collection starts before the plan and its check exist; amendment 27): ALLOCATION.json must have the digest TOPUP.json records and cover exactly S0-8's domains; per domain, refused unless the labels are exactly the config's and include gpt-6-1-terra, gpt-5-6-terra and gpt-6-astra; the counts sum to S0-8's add.frontier_records; an Enron domain (rule 2 waived) holds none; Terra (labels ending -terra) >= Astra (labels ending -astra); when F > 0, gpt-6-1-terra > gpt-5-6-terra; and every other label holds floor(F/n) or one more while the two Terra labels together hold at least 2 floor(F/n)";
/// The rule an added human's row records.
pub const HUMAN_ROW_RULE: &str = "a Train-mining human added to Train-2 by the top-up plan's rule 4 (r2_topup_plan_v1; protocol A2 section 6.4): frozen_split train_mining becomes train; the row is otherwise unchanged";

/// The frontier labels of the collector's plan config, in config order.
pub fn frontier_labels(cfg: &Value) -> Result<Vec<String>> {
    ensure!(cfg["schema"] == FRONTIER_CONFIG_SCHEMA, "the frontier config is not a {FRONTIER_CONFIG_SCHEMA}");
    let labels: Vec<String> = cfg["models"].as_array().context("frontier config models")?.iter()
        .map(|m| m["label"].as_str().map(String::from).context("a model without a label")).collect::<Result<_>>()?;
    let set: BTreeSet<&str> = labels.iter().map(String::as_str).collect();
    ensure!(set.len() == labels.len(), "the frontier config repeats a label");
    for l in [TERRA_61, TERRA_56, ASTRA] {
        ensure!(set.contains(l), "the frontier config has no {l} (amendment 5 item 5.3 names it)");
    }
    Ok(labels)
}

/// The allocation of `total` added records over `labels` ([`ALLOCATION_RULE`]).
pub fn allocate(total: u64, labels: &[String]) -> BTreeMap<String, u64> {
    let n = labels.len() as u64;
    let q = total / n;
    let mut r = total % n;
    let mut c: BTreeMap<String, u64> = labels.iter().map(|l| (l.clone(), q)).collect();
    let order = std::iter::once(TERRA_61).chain(labels.iter().map(String::as_str).filter(|l| *l != TERRA_61));
    for l in order {
        if r == 0 {
            break;
        }
        *c.get_mut(l).unwrap() += 1;
        r -= 1;
    }
    if total > 0 && c[TERRA_61] <= c[TERRA_56] {
        *c.get_mut(TERRA_56).unwrap() -= 1;
        *c.get_mut(TERRA_61).unwrap() += 1;
    }
    c
}

/// Refuses an allocation of one domain that breaks [`CHECK_RULE`].
pub fn check_allocation(domain: &str, total: u64, waived: bool, labels: &[String], alloc: &BTreeMap<String, u64>) -> Result<()> {
    let set: BTreeSet<&str> = labels.iter().map(String::as_str).collect();
    ensure!(alloc.keys().map(String::as_str).collect::<BTreeSet<_>>() == set, "domain {domain}: the allocation's labels are not the config's");
    for l in [TERRA_61, TERRA_56, ASTRA] {
        ensure!(set.contains(l), "domain {domain}: no {l} label");
    }
    let sum: u64 = alloc.values().sum();
    ensure!(sum == total, "domain {domain}: the allocation holds {sum} records, S0-8 adds {total}");
    if waived {
        ensure!(total == 0, "domain {domain}: rule 2 is waived for Enron and Enron text never goes to a hosted model (section 2.7), yet {total} frontier records are allocated");
    }
    let class = |suffix: &str| alloc.iter().filter(|(l, _)| l.ends_with(suffix)).map(|(_, n)| n).sum::<u64>();
    let (terra, astra) = (class("-terra"), class("-astra"));
    ensure!(terra >= astra, "domain {domain}: Terra holds {terra} records, Astra {astra} (Terra >= Astra, amendment 5 item 5.3)");
    if total > 0 {
        ensure!(alloc[TERRA_61] > alloc[TERRA_56], "domain {domain}: {TERRA_61} holds {}, {TERRA_56} {} (more to gpt-6.1-terra, section 4.5)", alloc[TERRA_61], alloc[TERRA_56]);
    }
    let q = total / labels.len() as u64;
    for (l, n) in alloc {
        if l != TERRA_61 && l != TERRA_56 {
            ensure!(*n == q || *n == q + 1, "domain {domain}: label {l} holds {n}, not {q} or {} (label-balanced)", q + 1);
        }
    }
    ensure!(alloc[TERRA_61] + alloc[TERRA_56] >= 2 * q, "domain {domain}: Terra's two labels hold less than their balanced share");
    Ok(())
}

/// One S0-8 domain's additions.
#[derive(Clone, Debug, Default)]
pub struct Adds {
    pub humans: u64,
    pub local_positives: u64,
    pub frontier_records: u64,
    pub floors: BTreeMap<String, u64>,
    pub enron: bool,
}

/// The additions of every S0-8 domain.
pub fn adds(s08: &Value) -> Result<BTreeMap<String, Adds>> {
    let enron: BTreeSet<&str> = s08["enron_domains"].as_array().map(|a| a.iter().filter_map(Value::as_str).collect()).unwrap_or_default();
    let mut out = BTreeMap::new();
    for (d, v) in s08["domains"].as_object().context("S0-8 domains")? {
        let a = &v["add"];
        let n = |k: &str| a[k].as_u64().with_context(|| format!("S0-8 domain {d}: add.{k}"));
        let mut floors = BTreeMap::new();
        if let Some(m) = a["local_for_generator_floor"].as_object() {
            for (l, x) in m {
                floors.insert(l.clone(), x.as_u64().with_context(|| format!("S0-8 domain {d}: floor of {l}"))?);
            }
        }
        out.insert(d.clone(), Adds { humans: n("humans")?, local_positives: n("local_positives")?, frontier_records: n("frontier_records")?, floors, enron: enron.contains(d.as_str()) });
    }
    for d in &enron {
        ensure!(out.contains_key(*d), "S0-8 names Enron domain {d}, which it does not cover");
    }
    Ok(out)
}

/// Reads id -> domain from a split manifest's lines of split `split` (read
/// from each line's `split` bytes; other lines skipped unparsed and counted).
fn manifest_domains(p: &Path, split: &str, skipped: &mut Skipped) -> Result<HashMap<String, String>> {
    let mut m = HashMap::new();
    let rd = BufReader::with_capacity(1 << 20, fs::File::open(p).with_context(|| p.display().to_string())?);
    for (n, l) in rd.split(b'\n').enumerate() {
        let l = l?;
        if l.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        match crate::renorm::line_split(&l, "split") {
            crate::renorm::LineSplit::One(s) if s == split => {
                let v: Value = serde_json::from_slice(&l).with_context(|| format!("{} line {}", p.display(), n + 1))?;
                let (Some(id), Some(d)) = (v["id"].as_str(), v["domain"].as_str()) else { bail!("{} line {}: no id or domain", p.display(), n + 1) };
                ensure!(m.insert(id.to_string(), d.to_string()).is_none(), "{}: id {id} listed twice", p.display());
            }
            crate::renorm::LineSplit::One(s) => *skipped.entry(format!("{}:manifest", p.display())).or_default().entry(s).or_default() += 1,
            _ => bail!("{} line {}: no split that can be read from the line's bytes; refused before any parse", p.display(), n + 1),
        }
    }
    Ok(m)
}

/// A candidate human: its line, id, S0-8 domain and V21 key.
struct Cand {
    line: usize,
    id: String,
    key: String,
}

/// The rows of a humans file of split `split` that pass `keep`, by line
/// number: `(line, row)` pairs are read in one pass and only the
/// metadata-light candidates are kept.
fn scan_humans(p: &Path, split: &str, skipped: &mut Skipped, mut f: impl FnMut(usize, &Value) -> Result<()>) -> Result<String> {
    let mut rd = BufReader::with_capacity(1 << 20, fs::File::open(p).with_context(|| p.display().to_string())?);
    let mut hasher = <sha2::Sha256 as sha2::Digest>::new();
    let mut buf = Vec::new();
    let mut n = 0usize;
    loop {
        buf.clear();
        if rd.read_until(b'\n', &mut buf)? == 0 {
            break;
        }
        sha2::Digest::update(&mut hasher, &buf);
        n += 1;
        let l = buf.strip_suffix(b"\n").unwrap_or(&buf);
        if l.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        match crate::renorm::line_split(l, "frozen_split") {
            crate::renorm::LineSplit::One(s) if s == split => {
                let v: Value = serde_json::from_slice(l).with_context(|| format!("{} line {n}", p.display()))?;
                f(n, &v)?;
            }
            crate::renorm::LineSplit::One(s) => *skipped.entry(format!("{}:humans", p.display())).or_default().entry(s).or_default() += 1,
            _ => bail!("{} line {n}: no frozen_split that can be read from the line's bytes; refused before any parse", p.display()),
        }
    }
    Ok(hex::encode(sha2::Digest::finalize(hasher)))
}

/// The lines `want` (line numbers) of `p`, byte for byte, in file order: the
/// second pass over a file that [`scan_humans`] read with digest `sha`. The
/// whole file is re-hashed as the first pass hashed it and must have that
/// digest, and each selected line's `frozen_split` is read from its bytes
/// and must be `split` before the line is kept (amendment 27).
fn lines_at(p: &Path, want: &BTreeSet<usize>, split: &str, sha: &str) -> Result<BTreeMap<usize, Vec<u8>>> {
    let mut rd = BufReader::with_capacity(1 << 20, fs::File::open(p).with_context(|| p.display().to_string())?);
    let mut hasher = <sha2::Sha256 as sha2::Digest>::new();
    let mut out = BTreeMap::new();
    let mut buf = Vec::new();
    let mut n = 0usize;
    loop {
        buf.clear();
        if rd.read_until(b'\n', &mut buf)? == 0 {
            break;
        }
        sha2::Digest::update(&mut hasher, &buf);
        n += 1;
        if want.contains(&n) {
            let l = buf.strip_suffix(b"\n").unwrap_or(&buf);
            match crate::renorm::line_split(l, "frozen_split") {
                crate::renorm::LineSplit::One(s) if s == split => {}
                _ => bail!("{} line {n}: no longer a {split} line on the second pass (the file changed while it was read); refused before any parse", p.display()),
            }
            out.insert(n, l.to_vec());
        }
    }
    ensure!(hex::encode(sha2::Digest::finalize(hasher)) == sha, "{} changed while it was read (its digest differs from the first pass's)", p.display());
    ensure!(out.len() == want.len(), "{} changed while it was read", p.display());
    Ok(out)
}

/// A human row's checks, as `plan::human_from` makes them for a Train row:
/// returns its id, planner domain (None for a collection with no planner
/// domain) and words.
fn human_meta(v: &Value) -> Result<(String, Option<&'static str>, usize)> {
    let mut r = v.clone();
    r["frozen_split"] = json!("train");
    let id = v["id"].as_str().context("id")?.to_string();
    match plan::human_from(&r, "train") {
        Ok(h) => Ok((id, Some(h.domain), h.word_count)),
        Err(e) if e.to_string().starts_with("unadmitted collection") => Ok((id, None, 0)),
        Err(e) => Err(e.context(format!("human {id}"))),
    }
}

/// The inputs of a top-up plan.
pub struct Inputs<'a> {
    pub values: &'a crate::values::Values,
    pub train_mining: &'a Path,
    pub train_mining_manifest: &'a Path,
    /// R0's Train humans and their manifest: the R0 Train ids that rule 4
    /// never adds again, and the second parent source (required).
    pub train: (&'a Path, &'a Path),
    pub configs: &'a Path,
    pub frontier_config: &'a Path,
}

/// The config `family` label of each Train instruct family in `dir`.
fn family_labels(dir: &Path) -> Result<(BTreeMap<String, &'static str>, Vec<Value>)> {
    let mut by_label: BTreeMap<String, &'static str> = BTreeMap::new();
    let mut rec = Vec::new();
    let mut ps: Vec<PathBuf> = fs::read_dir(dir)?.map(|e| e.map(|e| e.path())).collect::<Result<_, _>>()?;
    ps.sort();
    for p in ps.into_iter().filter(|p| p.extension().is_some_and(|x| x == "json")) {
        let b = fs::read(&p)?;
        let v: Value = serde_json::from_slice(&b).with_context(|| p.display().to_string())?;
        let Some(k) = v["key"].as_str() else { continue };
        if let Some(f) = plan::FAMILY_ORDER.iter().find(|f| **f == k) {
            let label = v["family"].as_str().with_context(|| format!("{}: no family", p.display()))?;
            ensure!(by_label.insert(label.to_string(), f).is_none(), "two Train instruct families share the config family {label}");
            rec.push(json!({"key": k, "family": label, "path": p, "sha256": text::sha_hex(&b)}));
        }
    }
    Ok((by_label, rec))
}

/// Writes the top-up plan to the new directory `out` (see the module notes).
pub fn run(inp: &Inputs, out: &Path) -> Result<Value> {
    ensure!(!out.exists(), "{} exists: the plan is a new directory", out.display());
    let (s08, s08_rec) = crate::values::supervision(inp.values)?;
    let adds = adds(&s08)?;
    let fc_b = fs::read(inp.frontier_config).with_context(|| inp.frontier_config.display().to_string())?;
    let labels = frontier_labels(&serde_json::from_slice(&fc_b)?)?;
    let (fam_of_label, configs_rec) = family_labels(inp.configs)?;
    ensure!(fam_of_label.len() == plan::FAMILY_ORDER.len(), "{}: configs of the seven Train instruct families are needed, found {}", inp.configs.display(), fam_of_label.len());
    let mut skipped = Skipped::new();
    // R0's Train humans: their ids, and parents by S0-8 domain
    let mut r0_ids: HashSet<String> = HashSet::new();
    let mut r0_cands: BTreeMap<String, Vec<Cand>> = BTreeMap::new();
    let (lo, hi) = grammar_a_form::window::LENGTH_WINDOW;
    let mut why: BTreeMap<String, BTreeMap<&str, usize>> = BTreeMap::new();
    let (tp, tm) = inp.train;
    let dom = manifest_domains(tm, "train", &mut skipped)?;
    let r0_sha = scan_humans(tp, "train", &mut skipped, |line, v| {
        let (id, pd, w) = human_meta(v)?;
        r0_ids.insert(id.clone());
        let Some(d) = dom.get(&id) else { return Ok(()) };
        let Some(a) = adds.get(d) else { return Ok(()) };
        if a.enron || pd.is_none() || w < lo || w > hi {
            return Ok(());
        }
        r0_cands.entry(d.clone()).or_default().push(Cand { line, key: span_ops::r2_key(KEY_PARENT, &id), id });
        Ok(())
    })?;
    let r0_rec = json!({"path": tp, "sha256": r0_sha, "manifest": {"path": tm, "sha256": text::sha_hex(&fs::read(tm)?)}, "humans": r0_ids.len()});
    // Train-mining candidates
    let tm_dom = manifest_domains(inp.train_mining_manifest, TRAIN_MINING, &mut skipped)?;
    let mut cands: BTreeMap<String, Vec<Cand>> = BTreeMap::new();
    let mut no_manifest = 0usize;
    let mut seen: HashSet<String> = HashSet::new();
    let tm_sha = scan_humans(inp.train_mining, TRAIN_MINING, &mut skipped, |line, v| {
        let (id, pd, w) = human_meta(v)?;
        ensure!(seen.insert(id.clone()), "Train-mining line {line}: a repeated human id");
        let Some(d) = tm_dom.get(&id) else {
            no_manifest += 1;
            return Ok(());
        };
        let enron_row = v["collection"] == grammar_a_pools::pile::ENRON_COLLECTION;
        let k = match adds.get(d) {
            None => "not_an_s0_8_domain",
            Some(_) if enron_row => "enron_row (the Train half is D6's)",
            Some(a) if a.enron => "enron_domain",
            Some(_) if r0_ids.contains(&id) => "already_an_r0_train_human",
            Some(_) if pd.is_none() => "collection_without_a_planner_domain",
            Some(_) if w < lo => "below_window",
            Some(_) if w > hi => "above_window",
            Some(_) => {
                cands.entry(d.clone()).or_default().push(Cand { line, key: span_ops::r2_key(KEY_HUMAN, &id), id });
                "candidate"
            }
        };
        *why.entry(d.clone()).or_default().entry(k).or_default() += 1;
        Ok(())
    })?;
    for v in cands.values_mut().chain(r0_cands.values_mut()) {
        v.sort_by(|a, b| a.key.cmp(&b.key).then_with(|| a.id.cmp(&b.id)));
    }
    // per domain: humans, parents, family quotas, frontier
    let mut doms = serde_json::Map::new();
    let mut quotas: BTreeMap<String, Vec<(&'static str, usize)>> = BTreeMap::new();
    let mut alloc_rec = serde_json::Map::new();
    let mut tm_lines: BTreeMap<usize, String> = BTreeMap::new();
    let mut r0_lines: BTreeMap<usize, String> = BTreeMap::new();
    let mut parent_order: Vec<(String, bool, usize)> = Vec::new();
    let mut totals = (0u64, 0u64, 0u64, BTreeMap::<String, u64>::new());
    for (d, a) in &adds {
        let mut short = Vec::new();
        let empty = Vec::new();
        let tc = if a.enron { &empty } else { cands.get(d).unwrap_or(&empty) };
        let rc = if a.enron { &empty } else { r0_cands.get(d).unwrap_or(&empty) };
        let h = (a.humans as usize).min(tc.len());
        if (h as u64) < a.humans {
            short.push(json!({"rule": "rule4_humans", "wanted": a.humans, "planned": h,
                "reason": if a.enron { "enron_humans_are_the_train_half (D6, S0-9)" } else { "train_mining_supply_ended" }}));
        }
        let added = &tc[..h];
        let want_l = a.local_positives as usize;
        let from_added = want_l.min(added.len());
        let from_r0 = (want_l - from_added).min(rc.len());
        let l = from_added + from_r0;
        if l < want_l {
            short.push(json!({"rule": "rule1_local_positives", "wanted": want_l, "planned": l,
                "reason": if a.enron { "enron_positives_are_d6s (the Train half)" } else { "parent_supply_ended" }}));
        }
        // rule-3 minimums by family
        let mut mins: BTreeMap<String, usize> = BTreeMap::new();
        let mut unmapped = BTreeMap::new();
        for (label, n) in &a.floors {
            match fam_of_label.get(label) {
                Some(f) => *mins.entry(f.to_string()).or_default() += *n as usize,
                None => {
                    unmapped.insert(label.clone(), *n);
                }
            }
        }
        let q = crate::train_mirror::family_quotas(l, &mins)?;
        for (f, m) in &mins {
            let got = q.iter().find(|x| x.0 == f).map(|x| x.1).unwrap_or(0);
            if got < *m {
                short.push(json!({"rule": "rule3_generator_floor", "family": f, "wanted": m, "planned": got, "reason": "parent_supply_ended"}));
            }
        }
        for (label, n) in &unmapped {
            short.push(json!({"rule": "rule3_generator_floor", "label": label, "wanted": n, "planned": 0,
                "reason": "the label is no Train instruct family's config family: its cell is not filled by the top-up (amendment 27)",
                "rows_planned_as": "local mirrors of the seven families in the equal split, counted toward rule 1 (S0-8 composes the rule-3 addition into add.local_positives)"}));
        }
        // frontier
        if a.enron {
            ensure!(a.frontier_records == 0, "S0-8 adds {} frontier records to Enron domain {d}, whose rule 2 is waived (section 2.7)", a.frontier_records);
        }
        let al = allocate(a.frontier_records, &labels);
        check_allocation(d, a.frontier_records, a.enron, &labels, &al)?;
        for (lb, n) in &al {
            *totals.3.entry(lb.clone()).or_default() += n;
        }
        alloc_rec.insert(d.clone(), json!({"records": a.frontier_records, "rule2_waived": a.enron, "by_label": al}));
        for c in added {
            tm_lines.insert(c.line, d.clone());
        }
        for c in &added[..from_added] {
            parent_order.push((d.clone(), true, c.line));
        }
        for c in &rc[..from_r0] {
            r0_lines.insert(c.line, d.clone());
            parent_order.push((d.clone(), false, c.line));
        }
        quotas.insert(d.clone(), q.clone());
        totals.0 += h as u64;
        totals.1 += l as u64;
        totals.2 += a.frontier_records;
        doms.insert(d.clone(), json!({
            "s0_8_add": {"humans": a.humans, "local_positives": a.local_positives, "frontier_records": a.frontier_records, "local_for_generator_floor": a.floors},
            "enron_domain": a.enron,
            "supply": {"train_mining": why.get(d), "r0_train_parents_in_window": rc.len()},
            "added_humans": h,
            "local_mirrors": {"planned": l, "parents_added_humans": from_added, "parents_r0_train": from_r0,
                "family_minimums": mins, "family_quotas": q.iter().map(|(f, n)| (f.to_string(), json!(n))).collect::<serde_json::Map<_, _>>()},
            "frontier": {"records": a.frontier_records, "by_label": al},
            "shortfalls": short}));
    }
    // the rows
    let tm_rows = lines_at(inp.train_mining, &tm_lines.keys().copied().collect(), TRAIN_MINING, &tm_sha)?;
    let mut rows_of: HashMap<usize, Vec<u8>> = HashMap::new();
    let mut humans_b = Vec::new();
    for (line, b) in &tm_rows {
        let mut v: Value = serde_json::from_slice(b)?;
        ensure!(v["frozen_split"] == TRAIN_MINING, "Train-mining line {line} changed while it was read");
        v["frozen_split"] = json!("train");
        v["split_assigned"] = json!(true);
        v["split_rule"] = json!(HUMAN_ROW_RULE);
        let mut o = serde_json::to_vec(&v)?;
        o.push(b'\n');
        humans_b.extend_from_slice(&o);
        rows_of.insert(*line, o);
    }
    let r0_rows = lines_at(tp, &r0_lines.keys().copied().collect(), "train", &r0_sha)?;
    let mut blocks: Vec<Value> = Vec::new();
    for d in quotas.keys() {
        let n = parent_order.iter().filter(|(x, _, _)| x == d).count();
        blocks.push(json!([d, n]));
    }
    let mut parents_b = Vec::new();
    for (_, tm, line) in &parent_order {
        if *tm {
            parents_b.extend_from_slice(&rows_of[line]);
        } else {
            parents_b.extend_from_slice(&r0_rows[line]);
            parents_b.push(b'\n');
        }
    }
    new_dir(out)?;
    new_dir(&out.join("frontier"))?;
    let hs = write_new(&out.join("humans.jsonl"), &humans_b)?;
    let ps = write_new(&out.join("parents.jsonl"), &parents_b)?;
    let alloc = json!({"schema": ALLOCATION_SCHEMA, "rule": ALLOCATION_RULE, "check": CHECK_RULE, "labels": labels,
        "frontier_config": {"path": inp.frontier_config, "sha256": text::sha_hex(&fc_b)}, "s0_8": s08_rec, "domains": alloc_rec,
        "totals_by_label": totals.3, "check_passed": true,
        "passages": "not selected here: Stage 0-H's top-up collector picks each domain's passages from the domain's Train-2 humans (the added humans first); its top-up mode is not built"});
    let mut ab = serde_json::to_vec_pretty(&alloc)?;
    ab.push(b'\n');
    let ah = write_new(&out.join("frontier").join("ALLOCATION.json"), &ab)?;
    let summary = json!({"schema": TOPUP_SCHEMA, "code": crate::CODE_ID, "rule": TOPUP_RULE, "s0_8": s08_rec,
        "inputs": {"train_mining": {"path": inp.train_mining, "sha256": tm_sha},
            "train_mining_manifest": {"path": inp.train_mining_manifest, "sha256": text::sha_hex(&fs::read(inp.train_mining_manifest)?)},
            "r0_train": r0_rec, "configs": configs_rec, "frontier_config": {"path": inp.frontier_config, "sha256": text::sha_hex(&fc_b)}},
        "keys": {"humans": {"x": KEY_HUMAN, "prefix": span_ops::R2_PLAN_KEY_PREFIX, "a6_key": false}, "r0_parents": {"x": KEY_PARENT, "prefix": span_ops::R2_PLAN_KEY_PREFIX, "a6_key": false}},
        "window": grammar_a_form::window::LENGTH_WINDOW_ID,
        "lines_of_other_splits_skipped_unparsed": skipped,
        "train_mining_without_a_manifest_domain": no_manifest,
        "totals": {"added_humans": totals.0, "local_mirrors": totals.1, "frontier_records": totals.2},
        "domains": doms,
        "family_quotas": quotas.iter().map(|(d, q)| (d.clone(), json!(q.iter().map(|(f, n)| (f.to_string(), json!(n))).collect::<serde_json::Map<_, _>>()))).collect::<serde_json::Map<_, _>>(),
        "outputs": {"humans": {"path": "humans.jsonl", "sha256": hs, "rows": tm_rows.len()}, "parents": {"path": "parents.jsonl", "sha256": ps, "rows": parent_order.len(), "blocks": blocks},
            "frontier_allocation": {"path": "frontier/ALLOCATION.json", "sha256": ah}},
        "local_part": "D5: train-mirror-plan --train <this>/parents.jsonl --topup <this> --values VALUES-R2.json --cal-from R0_LOCAL_PLAN --out D5 (the mirrors' r05 is S0-6's, read there; the register's S0-8 must be this plan's, and this plan's check is re-run there)",
        "train2_assembly": "humans.jsonl is a Train humans file of Train-2's assembly, with the Train-mining manifest among its domain manifests, beside any D5 mirror whose parent it holds (amendment 27; open for the assembly config)",
        "frontier_part": "Stage 0-H's top-up: frontier/ALLOCATION.json, checked here and by topup-check; no Stage 0-H collection starts before the plan and its check exist (amendment 5 item 5.3)",
        "refills": "a shortfall left by generation (rejections) is refilled by a later pass of the rules on the accepted rows; not built here"});
    let mut sb = serde_json::to_vec_pretty(&summary)?;
    sb.push(b'\n');
    let sh = write_new(&out.join("TOPUP.json"), &sb)?;
    let mut s = summary;
    s["topup_sha256"] = json!(sh);
    Ok(s)
}

/// The quota groups `train-mirror-plan --topup` reads from a top-up plan
/// whose `parents.jsonl` is `parents` (by digest): each parent's S0-8 domain
/// (the file's blocks, in the plan's `outputs.parents.blocks` order) and
/// each domain's family quotas. D5's step needs S0-8 in the register it is
/// given (protocol A2 §12.6: S0-8 precedes D5; §2.11), refused while absent,
/// and that S0-8 must be the one the top-up plan was written from; the
/// plan's check ([`check`]) is re-run against it. The parents file is read by
/// the Train-only planner's one-split reader (`train_mirror::read_train`:
/// each line's split from its bytes before any parse). Returns the groups,
/// the step's record and the parents file's digest, which the plan run
/// re-checks (amendment 27).
pub fn groups_of(topup: &Path, parents: &Path, values: &crate::values::Values) -> Result<(crate::train_mirror::Groups, Value, String)> {
    let (_, s08_rec) = crate::values::supervision(values)?;
    let b = fs::read(topup.join("TOPUP.json"))?;
    let t: Value = serde_json::from_slice(&b)?;
    ensure!(t["schema"] == TOPUP_SCHEMA, "{} is not a top-up plan", topup.display());
    ensure!(t["s0_8"]["value"] == s08_rec["value"], "the top-up plan was written from another S0-8 value than {}'s: D5 follows the register's S0-8 (protocol A2 section 12.6)", values.path.display());
    let ti = crate::train_mirror::read_train(parents)?;
    ensure!(t["outputs"]["parents"]["sha256"] == ti.sha256.as_str(), "{} is not the top-up plan's parents.jsonl", parents.display());
    let chk = check(topup, values)?;
    let mut q = BTreeMap::new();
    for (d, m) in t["family_quotas"].as_object().context("family_quotas")? {
        let mut v = Vec::new();
        for f in plan::FAMILY_ORDER {
            v.push((f, m[f].as_u64().with_context(|| format!("domain {d}: quota of {f}"))? as usize));
        }
        q.insert(d.clone(), v);
    }
    let ids: Vec<&str> = ti.humans.iter().map(|h| h.id.as_str()).collect();
    let mut of = HashMap::new();
    let mut at = 0usize;
    for blk in t["outputs"]["parents"]["blocks"].as_array().context("parents blocks")? {
        let d = blk[0].as_str().context("block domain")?;
        let n = blk[1].as_u64().context("block rows")? as usize;
        ensure!(q.get(d).is_some_and(|v: &Vec<(&str, usize)>| v.iter().map(|x| x.1).sum::<usize>() == n), "domain {d}: its block of {n} parents differs from its family quotas");
        for id in ids.get(at..at + n).context("parents.jsonl is shorter than its blocks")? {
            ensure!(of.insert(id.to_string(), d.to_string()).is_none(), "parent {id} is in two blocks");
        }
        at += n;
    }
    ensure!(at == ids.len(), "parents.jsonl holds {} rows, its blocks {at}", ids.len());
    let rec = json!({"step": "D5", "topup": topup, "topup_sha256": text::sha_hex(&b), "parents_sha256": ti.sha256, "s0_8": s08_rec,
        "topup_check": {"rule": CHECK_RULE, "passed": chk["passed"], "allocation_sha256": chk["allocation_sha256"]}});
    Ok(((of, q), rec, ti.sha256))
}

/// Re-checks a written top-up plan against S0-8 (`topup-check`; [`CHECK_RULE`]).
pub fn check(topup: &Path, values: &crate::values::Values) -> Result<Value> {
    let (s08, s08_rec) = crate::values::supervision(values)?;
    let adds = adds(&s08)?;
    let tb = fs::read(topup.join("TOPUP.json"))?;
    let t: Value = serde_json::from_slice(&tb)?;
    ensure!(t["schema"] == TOPUP_SCHEMA, "{} is not a top-up plan", topup.display());
    ensure!(t["s0_8"]["value"] == s08_rec["value"], "the plan was written from another S0-8 value");
    let ab = fs::read(topup.join("frontier").join("ALLOCATION.json"))?;
    ensure!(t["outputs"]["frontier_allocation"]["sha256"] == text::sha_hex(&ab).as_str(), "frontier/ALLOCATION.json differs from the plan's record");
    let a: Value = serde_json::from_slice(&ab)?;
    ensure!(a["schema"] == ALLOCATION_SCHEMA, "not a top-up frontier allocation");
    let labels: Vec<String> = a["labels"].as_array().context("labels")?.iter().map(|l| l.as_str().map(String::from).context("label")).collect::<Result<_>>()?;
    let doms = a["domains"].as_object().context("domains")?;
    ensure!(doms.keys().collect::<BTreeSet<_>>() == adds.keys().collect::<BTreeSet<_>>(), "the allocation's domains are not S0-8's");
    let mut out = serde_json::Map::new();
    for (d, ad) in &adds {
        let al: BTreeMap<String, u64> = doms[d]["by_label"].as_object().context("by_label")?.iter()
            .map(|(l, n)| Ok((l.clone(), n.as_u64().context("count")?))).collect::<Result<_>>()?;
        check_allocation(d, ad.frontier_records, ad.enron, &labels, &al)?;
        out.insert(d.clone(), json!({"records": ad.frontier_records, "passed": true}));
    }
    Ok(json!({"schema": CHECK_SCHEMA, "rule": CHECK_RULE, "topup_sha256": text::sha_hex(&tb), "allocation_sha256": text::sha_hex(&ab), "s0_8": s08_rec,
        "domains": out, "passed": true}))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn labels(extra: &[&str]) -> Vec<String> {
        let mut v: Vec<String> = [ASTRA, "gpt-6-1-sol", TERRA_61, "gpt-6-sol", TERRA_56].iter().map(|s| s.to_string()).collect();
        v.extend(extra.iter().map(|s| s.to_string()));
        v
    }

    #[test]
    fn the_allocation_meets_the_inequalities_at_every_total() {
        for ls in [labels(&[]), labels(&["claude-opus", "claude-fable"]), vec![ASTRA.into(), TERRA_61.into(), TERRA_56.into()]] {
            for f in 0..200u64 {
                let a = allocate(f, &ls);
                check_allocation("d", f, false, &ls, &a).unwrap_or_else(|e| panic!("{f} {ls:?}: {e}"));
                assert_eq!(a.values().sum::<u64>(), f);
            }
        }
        // hand-computed: 12 over 5 labels: 2 each, remainder 2 to 6.1-terra then astra; 6.1 (3) > 5.6 (2)
        let a = allocate(12, &labels(&[]));
        assert_eq!((a[TERRA_61], a[TERRA_56], a[ASTRA], a["gpt-6-1-sol"], a["gpt-6-sol"]), (3, 2, 3, 2, 2));
        // 10 over 5: 2 each, then one moves from 5.6 to 6.1
        let a = allocate(10, &labels(&[]));
        assert_eq!((a[TERRA_61], a[TERRA_56], a[ASTRA]), (3, 1, 2));
        // the remainder reaches 5.6 before the move: 4 over 3 labels -> 6.1 2, astra 1, 5.6 1
        let a = allocate(4, &[ASTRA.into(), TERRA_61.into(), TERRA_56.into()]);
        assert_eq!((a[TERRA_61], a[TERRA_56], a[ASTRA]), (2, 1, 1));
        assert_eq!(allocate(1, &labels(&[]))[TERRA_61], 1);
        assert!(allocate(0, &labels(&[])).values().all(|n| *n == 0));
    }

    #[test]
    fn the_check_refuses_each_broken_allocation() {
        let ls = labels(&[]);
        let good = allocate(20, &ls);
        check_allocation("d", 20, false, &ls, &good).unwrap();
        let bad = |f: &dyn Fn(&mut BTreeMap<String, u64>), total: u64, waived: bool, want: &str| {
            let mut a = allocate(total, &ls);
            f(&mut a);
            let e = check_allocation("d", total, waived, &ls, &a).err().unwrap_or_else(|| panic!("{want}: accepted")).to_string();
            assert!(e.contains(want), "{want}: {e}");
        };
        // 6.1 not above 5.6
        bad(&|a| { *a.get_mut(TERRA_61).unwrap() = 4; *a.get_mut(TERRA_56).unwrap() = 4; *a.get_mut(ASTRA).unwrap() = 4; *a.get_mut("gpt-6-1-sol").unwrap() = 4; *a.get_mut("gpt-6-sol").unwrap() = 4; }, 20, false, "more to gpt-6.1-terra");
        // Terra below Astra
        let three: Vec<String> = vec![ASTRA.into(), TERRA_61.into(), TERRA_56.into()];
        let mut a: BTreeMap<String, u64> = [(ASTRA.to_string(), 5), (TERRA_61.to_string(), 3), (TERRA_56.to_string(), 1)].into();
        assert!(check_allocation("d", 9, false, &three, &a).err().unwrap().to_string().contains("Terra >= Astra"));
        a.insert(ASTRA.into(), 3);
        a.insert(TERRA_61.into(), 5);
        check_allocation("d", 9, false, &three, &a).unwrap();
        // a sum other than S0-8's, an unbalanced label, a missing or extra label, Enron
        bad(&|a| *a.get_mut("gpt-6-sol").unwrap() += 1, 20, false, "S0-8 adds 20");
        bad(&|a| { *a.get_mut("gpt-6-sol").unwrap() += 2; *a.get_mut("gpt-6-1-sol").unwrap() -= 2; }, 20, false, "label-balanced");
        bad(&|a| { a.remove("gpt-6-sol"); }, 20, false, "labels are not the config's");
        bad(&|a| { a.insert("x".into(), 0); }, 20, false, "labels are not the config's");
        bad(&|_| {}, 3, true, "Enron text never goes to a hosted model");
        check_allocation("enron", 0, true, &ls, &allocate(0, &ls)).unwrap();
        // the config must name the three models
        assert!(frontier_labels(&json!({"schema": FRONTIER_CONFIG_SCHEMA, "models": [{"label": ASTRA}, {"label": TERRA_61}]})).is_err());
        assert!(frontier_labels(&json!({"schema": FRONTIER_CONFIG_SCHEMA, "models": [{"label": ASTRA}, {"label": TERRA_61}, {"label": TERRA_56}, {"label": ASTRA}]})).is_err());
        assert_eq!(frontier_labels(&json!({"schema": FRONTIER_CONFIG_SCHEMA, "models": [{"label": ASTRA}, {"label": TERRA_61}, {"label": TERRA_56}]})).unwrap().len(), 3);
        // an allocation that only the Terra pair's balanced share refuses: every other rule holds
        let a: BTreeMap<String, u64> = [(ASTRA, 2), ("gpt-6-1-sol", 3), (TERRA_61, 2), ("gpt-6-sol", 3), (TERRA_56, 0)].iter().map(|(l, n)| (l.to_string(), *n)).collect();
        let e = check_allocation("d", 10, false, &ls, &a).err().unwrap().to_string();
        assert!(e.contains("Terra's two labels hold less than their balanced share"), "{e}");
    }

    #[test]
    fn the_second_pass_rehashes_the_file_and_reads_each_selected_lines_split() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("h.jsonl");
        let body = b"{\"frozen_split\":\"train_mining\",\"id\":\"a\"}\n{\"frozen_split\":\"train\",\"id\":\"b\"}\n{\"frozen_split\":\"train_mining\",\"id\":\"c\"}\n";
        fs::write(&p, body).unwrap();
        let sha = text::sha_hex(body);
        let got = lines_at(&p, &[1, 3].into(), TRAIN_MINING, &sha).unwrap();
        assert_eq!(got.keys().copied().collect::<Vec<_>>(), [1, 3]);
        assert_eq!(got[&3], b"{\"frozen_split\":\"train_mining\",\"id\":\"c\"}".to_vec());
        // a file whose digest is not the first pass's
        let e = lines_at(&p, &[1].into(), TRAIN_MINING, &"0".repeat(64)).err().unwrap().to_string();
        assert!(e.contains("changed while it was read"), "{e}");
        // a selected line of another split (the line numbers of a changed file)
        let e = lines_at(&p, &[2].into(), TRAIN_MINING, &sha).err().unwrap().to_string();
        assert!(e.contains("no longer a train_mining line"), "{e}");
        // a line that no longer exists
        assert!(lines_at(&p, &[4].into(), TRAIN_MINING, &sha).is_err());
    }
}
