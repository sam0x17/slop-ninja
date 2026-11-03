//! Train-2 rows from the rows re-verdict (protocol A2 amendment 20 item 20.7;
//! BUILD-PLAN B5c, the round-2 coordinator's ruling R3; the readings of
//! amendment 22).
//!
//! `renorm-rows` (grammar-a-positives, `renorm::renorm_rows`) re-verdicts,
//! under profile `cc_cohort_profile_v2`, the Train rows of a bundle build that
//! no journal regenerates: RAID rows (an external-pool cohort `raid-v1-*`),
//! stage-3 legacy rows and hosted captures, each with model text. It writes a
//! directory holding:
//! * `ROWS-RENORM.json` (`slop_ninja_a_rows_renorm_v1`): the population, the
//!   prior's digest, the re-verdicted rows file, the counts and the two
//!   JSONL outputs' digests;
//! * `rows-reverdict.jsonl` (`slop_ninja_a_rows_reverdict_v1`): one receipt
//!   per planned row, no text: its source kind, origin, cohort, the digest of
//!   the row as re-verdicted (`raw_text_sha256`), the verdict's reasons and
//!   the action (`keep` or `drop_with_receipt`);
//! * `rows-normalised.jsonl` (`slop_ninja_a_rows_normalised_v1`): one line
//!   per kept row, with its laid-out `text`, `text_sha256`, `ai_byte_spans`
//!   and `share`, and the raw twin's `raw_text_sha256` and
//!   `raw_ai_byte_spans` (the bundle row as it was);
//! * `form-prior-<sha256>.json`, the rows batch's prior.
//!
//! With `round2.rows_renorm` naming that directory (`CONSUMER_RULE`), the
//! assembler:
//! 1. **Checks the directory** before it uses a line: the summary's schema,
//!    population (`train2`, the population R0's Train positives join,
//!    amendment 10 item 10.8), profile and output names; the two JSONL files
//!    and the prior against the summary's digests; then every line (schemas,
//!    one receipt per id, a normalised line for every `keep` receipt and none
//!    for a drop, the same digests, origin and prior on both, labels that
//!    agree with the origin, and the summary's counts). All four files are
//!    build inputs, so every manifest binds their digests.
//! 2. **Replaces and drops before selection** (as the Train candidates are
//!    gathered, before the selection rules, dedup and weights, so that the
//!    covariate bins, the Kish ESS and the RAID share see the laid-out rows).
//!    The Train-mining parents and the source groups that captures name are
//!    resolved first, from the candidates as gathered (`ORDER_RULE`).
//!    For every Train candidate whose id has a receipt:
//!    * the candidate must be a row with model text of those three kinds, of
//!      the receipt's source kind, origin and cohort, and its own text must
//!      hash to the receipt's `raw_text_sha256`, or the build is refused;
//!    * a `drop_with_receipt` candidate is excluded (`DROP_REASON`) with the
//!      receipt's file digest, line and reasons;
//!    * a `keep` candidate, whose spans must equal the normalised line's
//!      `raw_ai_byte_spans`, takes that line's `text`, `text_sha256`,
//!      `ai_byte_spans` and `share`; origin and every other field are kept;
//!    * a `keep` candidate whose raw text is an evaluation text is excluded
//!      (`RAW_EVAL_REASON`, `RAW_EVAL_RULE`), since its raw twin would repeat
//!      that text.
//! 3. **Deduplicates laid-out rows with their raw twins** (`RAW_TWIN_RULE`;
//!    a laid-out row and its raw twin are one document). At selection's
//!    dedup step, in selection's order (humans first, then by source kind and
//!    id), a laid-out row also holds its raw text: one whose raw text an
//!    earlier kept row holds is excluded (`RAW_DUP_REASON`), and a kept one
//!    marks both texts as seen, so a later row with its raw text is a
//!    `duplicate_text` of it, as in R0's selection. A candidate of those three
//!    kinds with model text and no receipt whose text is any planned row's
//!    raw text (kept or dropped) is that row's raw twin and is excluded
//!    (`RAW_TWIN_REASON`), naming the planned row and its action.
//! 4. **Refuses unplanned positives after selection** (`UNPLANNED_RULE`,
//!    BUILD-PLAN 10.5 open item (a), read for selected rows): a selected Train
//!    row with model text of those three kinds that holds no laid-out text
//!    refuses the build. A candidate that the rows re-verdict did not plan and
//!    selection drops for its own reason is not refused. `renorm-rows` plans
//!    external-pool rows of the `raid-v1-*` cohorts only, so a selected
//!    positive of another external pool is always refused.
//!
//! The Train manifest records it all in `rows_renorm` (counts and digests, no
//! text). The option absent, nothing here runs and the build's bytes are
//! unchanged.
use crate::bundle::{Inputs, file_sha};
use crate::{Exclusion, Row, SourceKind, check_labels, sha_hex, spans_of, str_req};
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

/// `renorm::ROWS_RENORM_SCHEMA` (grammar-a-positives).
pub const SUMMARY_SCHEMA: &str = "slop_ninja_a_rows_renorm_v1";
/// `renorm::ROWS_REVERDICT_SCHEMA`.
pub const REVERDICT_SCHEMA: &str = "slop_ninja_a_rows_reverdict_v1";
/// `renorm::ROWS_NORMALISED_SCHEMA`.
pub const NORMALISED_SCHEMA: &str = "slop_ninja_a_rows_normalised_v1";
pub const SUMMARY_FILE: &str = "ROWS-RENORM.json";
pub const REVERDICT_FILE: &str = "rows-reverdict.jsonl";
pub const NORMALISED_FILE: &str = "rows-normalised.jsonl";
/// `grammar_a_form::profile::PROFILE_V2`.
pub const PROFILE_V2: &str = "cc_cohort_profile_v2";
/// The population that R0's Train positives join (amendment 10 item 10.8).
pub const POPULATION: &str = "train2";
pub const ACTION_KEEP: &str = "keep";
pub const ACTION_DROP: &str = "drop_with_receipt";
/// The exclusion reason of a planned row that the rows re-verdict drops.
pub const DROP_REASON: &str = "rows_renorm_drop_with_receipt";
/// The exclusion reason of a kept row whose raw text is an evaluation text.
pub const RAW_EVAL_REASON: &str = "rows_renorm_raw_text_in_evaluation_split";
/// The exclusion reason of an unplanned candidate whose text is a planned row's raw text.
pub const RAW_TWIN_REASON: &str = "rows_renorm_duplicate_of_planned_raw_text";
/// The exclusion reason of a laid-out row whose raw text an earlier kept row holds.
pub const RAW_DUP_REASON: &str = "rows_renorm_raw_text_duplicate";
/// The source kinds whose Train rows the rows re-verdict covers.
pub const SCOPE: [SourceKind; 3] = [SourceKind::ExternalPool, SourceKind::Stage3Legacy, SourceKind::HostedCapture];

/// What the option does (amendment 20 item 20.7; ruling R3).
pub const CONSUMER_RULE: &str = "round2.rows_renorm (amendment 20 item 20.7; ruling R3) names a renorm-rows output directory (ROWS-RENORM.json, rows-reverdict.jsonl, rows-normalised.jsonl and its prior, checked against the summary's digests and bound as inputs); as the Train candidates are gathered, before selection, dedup and weights, every candidate whose bundle row id has a receipt in rows-reverdict.jsonl must be a row with model text of the receipt's source kind (external_pool, stage3_legacy or hosted_capture), origin and cohort, whose text hashes to the receipt's raw_text_sha256 (the row as re-verdicted), or the build is refused; a drop_with_receipt candidate is excluded with its receipt; a keep candidate takes the text, text_sha256, ai_byte_spans and share of its rows-normalised.jsonl line, its spans equal to that line's raw_ai_byte_spans, and keeps origin and every other field";
/// BUILD-PLAN 10.5 open item (a), read for selected rows (fail closed; amendment 22).
pub const UNPLANNED_RULE: &str = "every Train-2 positive is under profile v2 (protocol A2 section 6.2; amendment 22): a selected Train row with model text of source kind external_pool, stage3_legacy or hosted_capture that holds no laid-out text from rows-normalised.jsonl refuses the build; a candidate with no receipt that selection drops for its own reason is not refused; renorm-rows plans external_pool rows of raid-v1-* cohorts only, so a selected positive of any other external pool is refused";
/// A kept row's raw twin (the bundle row as it was) is checked like its laid-out text (amendment 22).
pub const RAW_EVAL_RULE: &str = "a keep candidate whose raw text (its raw twin, the bundle row as it was) is an evaluation text is excluded, as selection excludes a Train row whose text is one (protocol A2 section 6.1; amendment 22)";
/// An unplanned candidate whose text is a planned row's raw text is that row's raw twin (amendment 22).
pub const RAW_TWIN_RULE: &str = "a laid-out row and its raw twin are one document at selection's dedup step, in selection's order: a laid-out row whose raw text (raw_text_sha256) an earlier kept row holds as its text or raw text is excluded (rows_renorm_raw_text_duplicate), and a kept laid-out row marks its text and its raw text as seen, so a later row with its raw text is a duplicate_text of it, as in R0's selection; after those checks, a Train candidate with model text of source kind external_pool, stage3_legacy or hosted_capture that has no receipt in rows-reverdict.jsonl and whose text_sha256 is any receipt's raw_text_sha256 (kept or dropped) is that planned row's raw twin and is excluded (rows_renorm_duplicate_of_planned_raw_text), naming the planned row and its action; rows are otherwise deduplicated by their own text (amendment 22)";
/// When the rows re-verdict is applied (amendment 22).
pub const ORDER_RULE: &str = "the Train-mining parents of Train positives and the source groups that hosted captures name are resolved from the Train candidates as gathered, before the rows re-verdict replaces or drops any; replacement changes no source group, domain, source kind or parent, and a planned row that the re-verdict drops still names its source group (the stage-3 legacy file is Train whatever a row's verdict) (amendment 22)";

fn is_sha256(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// The serialised source-kind name (`external_pool`, ...), as bundle rows carry it.
fn kind_name(k: SourceKind) -> String {
    serde_json::to_value(k).ok().and_then(|v| v.as_str().map(String::from)).unwrap_or_default()
}

/// The manifest's source-kind key (`rows_by_source_kind` spelling).
fn kind_key(r: &Row) -> String {
    format!("{:?}", r.source_kind)
}

/// Whether a row is one the rows re-verdict covers: model text of a scope kind.
pub fn in_scope(r: &Row) -> bool {
    SCOPE.contains(&r.source_kind) && r.origin != "human_only"
}

/// One receipt line of `rows-reverdict.jsonl`.
#[derive(Clone, Debug)]
struct Receipt {
    line: usize,
    keep: bool,
    source_kind: String,
    origin: String,
    cohort_id: String,
    raw_text_sha256: String,
    text_sha256: Option<String>,
    reasons: Value,
    class: Value,
}

/// One line of `rows-normalised.jsonl`.
#[derive(Clone, Debug)]
struct Normalised {
    line: usize,
    text: String,
    text_sha256: String,
    spans: Vec<[usize; 2]>,
    share: f64,
    raw_spans: Vec<[usize; 2]>,
}

/// A checked rows re-verdict directory.
#[derive(Debug)]
pub struct RowsRenorm {
    pub dir: PathBuf,
    files: BTreeMap<String, String>,
    prior_sha256: String,
    reverdict_sha256: String,
    re_verdicted_rows: Value,
    receipts: HashMap<String, Receipt>,
    normalised: HashMap<String, Normalised>,
}

impl RowsRenorm {
    /// Read and check the directory (step 1 of the module notes), recording
    /// its four files as build inputs.
    pub fn read(dir: &Path, inputs: &mut Inputs) -> Result<RowsRenorm> {
        let at = |f: &str| dir.join(f);
        let s = inputs.json(&at(SUMMARY_FILE), "rows_renorm_summary")?;
        let mut files = BTreeMap::new();
        files.insert(SUMMARY_FILE.to_string(), inputs.list.last().unwrap().sha256.clone());
        ensure!(s["schema"] == SUMMARY_SCHEMA, "{SUMMARY_FILE}: schema must be {SUMMARY_SCHEMA}");
        ensure!(s["population"] == POPULATION,
            "{SUMMARY_FILE}: population {} is not {POPULATION}, the population R0's Train positives join (protocol A2 amendment 10 item 10.8)", s["population"]);
        ensure!(s["profile"]["profile"] == PROFILE_V2, "{SUMMARY_FILE}: profile {} is not {PROFILE_V2}", s["profile"]["profile"]);
        let prior_sha256 = str_req(&s, "prior_sha256").context(SUMMARY_FILE)?.to_string();
        ensure!(is_sha256(&prior_sha256), "{SUMMARY_FILE}: prior_sha256 is not a SHA-256");
        let out = &s["outputs"];
        let prior_name = format!("form-prior-{prior_sha256}.json");
        ensure!(out["prior"] == prior_name.as_str(), "{SUMMARY_FILE}: outputs.prior is not {prior_name}");
        ensure!(out["reverdict"]["path"] == REVERDICT_FILE && out["normalised"]["path"] == NORMALISED_FILE,
            "{SUMMARY_FILE}: outputs name other files than {REVERDICT_FILE} and {NORMALISED_FILE}");
        let want_rv = str_req(&out["reverdict"], "sha256").context(SUMMARY_FILE)?.to_string();
        let want_nm = str_req(&out["normalised"], "sha256").context(SUMMARY_FILE)?.to_string();
        // Every file against the summary before any of its lines is used.
        for (f, want) in [(REVERDICT_FILE, &want_rv), (NORMALISED_FILE, &want_nm)] {
            let got = file_sha(&at(f))?;
            ensure!(&got == want, "{f} hashes to {got}, not {SUMMARY_FILE}'s {want}: refused");
        }
        inputs.json(&at(&prior_name), "rows_renorm_prior")?;
        let got = inputs.list.last().unwrap().sha256.clone();
        ensure!(got == prior_sha256, "{prior_name} hashes to {got}: refused");
        files.insert(prior_name, got);
        let mut receipts: HashMap<String, Receipt> = HashMap::new();
        inputs.jsonl(&at(REVERDICT_FILE), "rows_renorm_reverdict", |_, line, _, v| {
            ensure!(v["schema"] == REVERDICT_SCHEMA, "schema must be {REVERDICT_SCHEMA}");
            let id = str_req(&v, "id")?;
            let keep = match str_req(&v, "action")? {
                ACTION_KEEP => true,
                ACTION_DROP => false,
                a => bail!("row {id}: unknown action {a}"),
            };
            ensure!(v["pass"] == keep, "row {id}: pass and action differ");
            let raw = str_req(&v, "raw_text_sha256")?;
            ensure!(is_sha256(raw), "row {id}: raw_text_sha256 is not a SHA-256");
            let text_sha256 = v["text_sha256"].as_str().map(String::from);
            let reasons = v["reasons"].clone();
            ensure!(reasons.as_array().is_some_and(|a| a.iter().all(Value::is_string)), "row {id}: reasons must be strings");
            if keep {
                ensure!(text_sha256.is_some() && reasons.as_array().unwrap().is_empty(), "row {id}: a kept row needs its text digest and no reason");
            }
            let r = Receipt { line, keep, source_kind: str_req(&v, "source_kind")?.into(), origin: str_req(&v, "origin")?.into(),
                cohort_id: v["cohort_id"].as_str().unwrap_or_default().into(), raw_text_sha256: raw.into(), text_sha256, reasons,
                class: v["class"].clone() };
            ensure!(receipts.insert(id.to_string(), r).is_none(), "row {id} has two receipts");
            Ok(())
        })?;
        // The re-verdicted rows are one bundle's Train rows, unique by text.
        let mut raw_ids: HashMap<&str, &str> = HashMap::new();
        for (id, r) in &receipts {
            if let Some(other) = raw_ids.insert(r.raw_text_sha256.as_str(), id.as_str()) {
                bail!("rows {other} and {id} of {REVERDICT_FILE} have the same raw_text_sha256 {}: the re-verdicted rows are one bundle's Train rows, unique by text; refused", r.raw_text_sha256);
            }
        }
        let got = inputs.list.last().unwrap().sha256.clone();
        ensure!(got == want_rv, "{REVERDICT_FILE} changed while it was read");
        files.insert(REVERDICT_FILE.into(), got);
        let mut normalised: HashMap<String, Normalised> = HashMap::new();
        inputs.jsonl(&at(NORMALISED_FILE), "rows_renorm_normalised", |_, line, _, v| {
            ensure!(v["schema"] == NORMALISED_SCHEMA, "schema must be {NORMALISED_SCHEMA}");
            let id = str_req(&v, "id")?;
            let rc = receipts.get(id).with_context(|| format!("row {id} is normalised but has no receipt in {REVERDICT_FILE}"))?;
            ensure!(rc.keep, "row {id} is normalised but its receipt drops it");
            ensure!(v["profile"] == PROFILE_V2 && v["population"] == POPULATION && v["prior_sha256"] == prior_sha256.as_str(),
                "row {id}: profile, population or prior differ from {SUMMARY_FILE}'s");
            let text = str_req(&v, "text")?.to_string();
            let d = sha_hex(text.as_bytes());
            ensure!(v["text_sha256"] == d.as_str() && rc.text_sha256.as_deref() == Some(d.as_str()),
                "row {id}: the text, its text_sha256 and the receipt's text_sha256 differ");
            ensure!(v["origin"] == rc.origin.as_str() && v["raw_text_sha256"] == rc.raw_text_sha256.as_str(),
                "row {id}: origin or raw_text_sha256 differ from its receipt");
            let spans = spans_of(&v["ai_byte_spans"]).with_context(|| format!("row {id}: ai_byte_spans"))?;
            let share = v["share"].as_f64().with_context(|| format!("row {id}: share"))?;
            check_labels(&rc.origin, share, &spans, &text).with_context(|| format!("row {id}: normalised labels"))?;
            let raw_spans = spans_of(&v["raw_ai_byte_spans"]).with_context(|| format!("row {id}: raw_ai_byte_spans"))?;
            let n = Normalised { line, text, text_sha256: d, spans, share, raw_spans };
            ensure!(normalised.insert(id.to_string(), n).is_none(), "row {id} is normalised twice");
            Ok(())
        })?;
        let got = inputs.list.last().unwrap().sha256.clone();
        ensure!(got == want_nm, "{NORMALISED_FILE} changed while it was read");
        files.insert(NORMALISED_FILE.into(), got);
        let keeps = receipts.values().filter(|r| r.keep).count();
        ensure!(keeps == normalised.len(), "{keeps} rows are kept in {REVERDICT_FILE}, {} in {NORMALISED_FILE}", normalised.len());
        let c = &s["counts"];
        ensure!(c["planned"].as_u64() == Some(receipts.len() as u64) && c["kept"].as_u64() == Some(normalised.len() as u64),
            "{SUMMARY_FILE}: counts planned {} and kept {} differ from the files' {} and {}", c["planned"], c["kept"], receipts.len(), normalised.len());
        Ok(RowsRenorm { dir: dir.into(), files, prior_sha256, reverdict_sha256: want_rv, re_verdicted_rows: s["inputs"]["rows"].clone(),
            receipts, normalised })
    }

    /// Replace and drop the Train candidates (step 2 of the module notes).
    /// `eval_texts` maps every evaluation text digest to its split.
    pub fn apply(&self, train: &mut Vec<Row>, eval_texts: &HashMap<String, String>) -> Result<Applied> {
        let mut a = Applied::default();
        let mut replaced: BTreeMap<String, usize> = BTreeMap::new();
        let mut dropped: BTreeMap<String, usize> = BTreeMap::new();
        let mut raw_eval: BTreeMap<String, usize> = BTreeMap::new();
        let mut unplanned: BTreeMap<String, usize> = BTreeMap::new();
        let mut gathered: HashSet<&str> = HashSet::new();
        let mut out = Vec::with_capacity(train.len());
        for mut r in std::mem::take(train) {
            let Some((rid, rc)) = self.receipts.get_key_value(&r.id) else {
                if in_scope(&r) {
                    *unplanned.entry(kind_key(&r)).or_default() += 1;
                }
                out.push(r);
                continue;
            };
            let id = r.id.clone();
            ensure!(in_scope(&r), "{REVERDICT_FILE} line {} names Train candidate {id}, a {:?} {} row: the rows re-verdict covers only rows with model text of source kind external_pool, stage3_legacy or hosted_capture",
                rc.line, r.source_kind, r.origin);
            ensure!(kind_name(r.source_kind) == rc.source_kind && r.origin == rc.origin && r.cohort_id == rc.cohort_id,
                "row {id}: {REVERDICT_FILE} line {} names source kind {}, origin {} and cohort {}; the candidate has {}, {} and {}",
                rc.line, rc.source_kind, rc.origin, rc.cohort_id, kind_name(r.source_kind), r.origin, r.cohort_id);
            let raw = sha_hex(r.text.as_bytes());
            ensure!(raw == rc.raw_text_sha256,
                "row {id}: the candidate's text hashes to {raw}, not the raw_text_sha256 {} of {REVERDICT_FILE} line {} (the row as re-verdicted): refused",
                rc.raw_text_sha256, rc.line);
            gathered.insert(rid.as_str());
            if !rc.keep {
                *dropped.entry(kind_key(&r)).or_default() += 1;
                a.drops.push(Exclusion { id, split: "train".into(), source_kind: r.source_kind, reason: DROP_REASON.into(),
                    detail: json!({"receipt": {"file": REVERDICT_FILE, "sha256": self.reverdict_sha256, "line": rc.line},
                        "reasons": rc.reasons, "class": rc.class}) });
                continue;
            }
            let n = self.normalised.get(&r.id).with_context(|| format!("row {id}: a keep receipt without its {NORMALISED_FILE} line"))?;
            ensure!(n.raw_spans == r.ai_byte_spans, "row {id}: raw_ai_byte_spans of {NORMALISED_FILE} line {} differ from the candidate's spans", n.line);
            if let Some(s) = eval_texts.get(&raw) {
                *raw_eval.entry(kind_key(&r)).or_default() += 1;
                a.drops.push(Exclusion { id, split: "train".into(), source_kind: r.source_kind, reason: RAW_EVAL_REASON.into(),
                    detail: json!({"evaluation_split": s, "raw_text_sha256": raw, "rule": RAW_EVAL_RULE}) });
                continue;
            }
            r.text = n.text.clone();
            r.text_sha256 = n.text_sha256.clone();
            r.ai_byte_spans = n.spans.clone();
            r.share = n.share;
            *replaced.entry(kind_key(&r)).or_default() += 1;
            a.replaced_raw.insert(id, raw);
            out.push(r);
        }
        *train = out;
        let mut without: BTreeMap<&str, usize> = BTreeMap::new();
        for (id, rc) in &self.receipts {
            if !gathered.contains(id.as_str()) {
                *without.entry(if rc.keep { ACTION_KEEP } else { ACTION_DROP }).or_default() += 1;
            }
        }
        a.raw_twins = self.receipts.iter()
            .map(|(id, rc)| (rc.raw_text_sha256.clone(), (id.clone(), if rc.keep { ACTION_KEEP } else { ACTION_DROP })))
            .collect();
        a.planned = self.receipts.keys().cloned().collect();
        a.receipts_sha256 = self.reverdict_sha256.clone();
        let keeps = self.normalised.len();
        a.report = json!({
            "rule": CONSUMER_RULE, "unplanned_rule": UNPLANNED_RULE, "raw_text_rule": RAW_EVAL_RULE,
            "raw_twin_rule": RAW_TWIN_RULE, "order_rule": ORDER_RULE,
            "directory": self.dir, "files": self.files, "population": POPULATION, "profile": PROFILE_V2,
            "prior_sha256": self.prior_sha256, "re_verdicted_rows": self.re_verdicted_rows,
            "receipts": {"planned": self.receipts.len(), "keep": keeps, "drop_with_receipt": self.receipts.len() - keeps},
            "candidates": {"replaced": replaced, "dropped_with_receipt": dropped, "raw_text_in_evaluation_split": raw_eval,
                "in_scope_without_receipt": unplanned},
            "receipts_without_a_candidate": without,
        });
        Ok(a)
    }
}

/// The result of `RowsRenorm::apply`.
#[derive(Debug, Default)]
pub struct Applied {
    /// Exclusions of the Train split (dropped with a receipt, or a raw evaluation text).
    pub drops: Vec<Exclusion>,
    /// The laid-out candidates' ids, with their raw text digests.
    replaced_raw: HashMap<String, String>,
    /// Every receipt's raw text digest, with its row id and action.
    raw_twins: HashMap<String, (String, &'static str)>,
    /// The ids that have a receipt.
    planned: HashSet<String>,
    receipts_sha256: String,
    report: Value,
}

impl Applied {
    /// Step 3 of the module notes (`RAW_TWIN_RULE`), for `select_with`'s
    /// dedup step: the raw text digest of a laid-out candidate.
    pub fn replaced_raw(&self, id: &str) -> Option<&str> {
        self.replaced_raw.get(id).map(String::as_str)
    }

    /// Step 3 of the module notes (`RAW_TWIN_RULE`), for `select_with`'s
    /// dedup step: the exclusion detail when `r` is an unplanned candidate of
    /// the three kinds whose text is a planned row's raw text.
    pub fn raw_twin_of(&self, r: &Row) -> Option<Value> {
        if !in_scope(r) || self.planned.contains(&r.id) {
            return None;
        }
        self.raw_twins.get(&r.text_sha256).map(|(id, action)| json!({"planned_row": id, "planned_action": action,
            "raw_text_sha256": r.text_sha256, "receipts": {"file": REVERDICT_FILE, "sha256": self.receipts_sha256}, "rule": RAW_TWIN_RULE}))
    }

    /// Rule (a) on the selected Train rows (step 4 of the module notes).
    /// `ex` holds selection's exclusions. Returns the Train manifest's
    /// `rows_renorm` record.
    pub fn check_selected(&self, selected: &[Row], ex: &[Exclusion]) -> Result<Value> {
        let mut kept: BTreeMap<String, usize> = BTreeMap::new();
        let mut unplanned: Vec<&Row> = Vec::new();
        for r in selected.iter().filter(|r| in_scope(r)) {
            if self.replaced_raw.contains_key(&r.id) {
                *kept.entry(kind_key(r)).or_default() += 1;
            } else {
                unplanned.push(r);
            }
        }
        if let Some(f) = unplanned.first() {
            bail!("{} selected Train rows have model text of a rows re-verdict source kind and no laid-out text from {NORMALISED_FILE} (first: {} {:?}, cohort {}); refused: {UNPLANNED_RULE}",
                unplanned.len(), f.id, f.source_kind, f.cohort_id);
        }
        let by_kind = |reason: &str| {
            let mut m: BTreeMap<String, usize> = BTreeMap::new();
            for e in ex.iter().filter(|e| e.reason == reason) {
                *m.entry(format!("{:?}", e.source_kind)).or_default() += 1;
            }
            m
        };
        let mut rec = self.report.clone();
        rec["selected"] = json!({"replaced_rows": kept, "rows_without_laid_out_text": 0,
            "raw_twins_of_planned_rows_excluded": by_kind(RAW_TWIN_REASON), "raw_text_duplicates_excluded": by_kind(RAW_DUP_REASON)});
        Ok(rec)
    }
}
