//! Admit an exact, pinned allow-list of original historical Train roots.
//! admit_train_expansion ADMISSION_PLAN.json NEW_OUT
//! No acquisition, generation, Test shards, cropping, or replacement candidates.
#![recursion_limit = "256"]
use anyhow::{Context, Result, ensure};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
};
#[allow(dead_code)]
#[path = "prepare_supervised_forest/supervision.rs"]
mod supervision;

const PARTS: [&str; 3] = ["train", "development", "calibration"];
const COLLECTIONS: [&str; 4] = [
    "cmu-books-historical",
    "hc3-wiki-historical",
    "plos_one",
    "wikinews",
];
const RANK_DOMAIN: &str = "slop_ninja_train_augmentation_roots_v1\0";
const PLAN_SCHEMA: &str = "slop_ninja_retained_train_admission_plan_v1";

fn sha(b: &[u8]) -> String {
    hex::encode(Sha256::digest(b))
}
fn str_at<'a>(v: &'a Value, k: &str) -> Result<&'a str> {
    v[k].as_str()
        .filter(|s| !s.is_empty())
        .with_context(|| format!("missing {k}"))
}
fn counter(v: &Value, k: &str) -> Result<u64> {
    v[k].as_u64().with_context(|| format!("counter {k}"))
}
fn number(v: &Value) -> Result<f64> {
    let n = v.as_f64().context("number")?;
    ensure!(n.is_finite() && n >= 0., "invalid number");
    Ok(n)
}
fn valid_sha(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
}
fn digest_at<'a>(v: &'a Value, k: &str) -> Result<&'a str> {
    let s = str_at(v, k)?;
    ensure!(valid_sha(s), "invalid digest {k}");
    Ok(s)
}
fn array<'a>(v: &'a Value, k: &str) -> Result<&'a Vec<Value>> {
    v[k].as_array().with_context(|| format!("array {k}"))
}
fn strings(v: &Value, k: &str) -> Result<BTreeSet<String>> {
    let mut out = BTreeSet::new();
    for x in array(v, k)? {
        let s = x.as_str().context("string ID")?;
        ensure!(
            !s.is_empty() && out.insert(s.to_owned()),
            "empty/duplicate {k}"
        );
    }
    Ok(out)
}
fn read(p: &Path) -> Result<Vec<u8>> {
    let mut b = Vec::new();
    File::open(p)
        .with_context(|| format!("open {}", p.display()))?
        .take(128 * 1024 * 1024 + 1)
        .read_to_end(&mut b)?;
    ensure!(b.len() <= 128 * 1024 * 1024, "input exceeds bound");
    Ok(b)
}
fn put(p: &Path, b: &[u8]) -> Result<()> {
    let mut f = OpenOptions::new().write(true).create_new(true).open(p)?;
    f.write_all(b)?;
    f.sync_all()?;
    Ok(())
}
fn encode_rows(rows: &[Value]) -> Result<Vec<u8>> {
    let mut b = Vec::new();
    for r in rows {
        serde_json::to_writer(&mut b, r)?;
        b.push(b'\n');
    }
    Ok(b)
}
fn parse_rows(b: &[u8]) -> Result<Vec<Value>> {
    std::str::from_utf8(b)?
        .lines()
        .map(|l| Ok(serde_json::from_str(l)?))
        .collect()
}
fn norm_sha(s: &str) -> String {
    sha(s
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .as_bytes())
}
fn rank(s: &str) -> String {
    sha(format!("{RANK_DOMAIN}{s}").as_bytes())
}

struct Inputs {
    base: PathBuf,
    pins: BTreeMap<String, Value>,
}
impl Inputs {
    fn pinned(&mut self, pin: &Value) -> Result<Vec<u8>> {
        let p = self.base.join(str_at(pin, "path")?);
        let expected = digest_at(pin, "sha256")?;
        let b = read(&p)?;
        ensure!(sha(&b) == expected, "pinned input changed: {}", p.display());
        self.pins.insert(
            p.display().to_string(),
            json!({"sha256":expected,"bytes":b.len()}),
        );
        Ok(b)
    }
    fn json(&mut self, pin: &Value) -> Result<Value> {
        Ok(serde_json::from_slice(&self.pinned(pin)?)?)
    }
}

// Both the original selector's punctuation-as-space keys and apostrophe-elided
// keys are retained. Main titles and explicit aliases group editions/volumes.
fn title_keys(s: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for raw in [s, s.split([':', ';', '(']).next().unwrap_or(s)] {
        for elide in [false, true] {
            let t = raw
                .to_lowercase()
                .chars()
                .filter(|c| !elide || !['\'', '’'].contains(c))
                .map(|c| if c.is_alphanumeric() { c } else { ' ' })
                .collect::<String>();
            let mut words = t.split_whitespace().collect::<Vec<_>>();
            if words
                .first()
                .is_some_and(|w| ["a", "an", "the"].contains(w))
            {
                words.remove(0);
            }
            let key = words.join(" ");
            if !key.is_empty() {
                out.insert(key);
            }
        }
    }
    out
}
fn author_key(s: &str) -> String {
    let t = s
        .to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { ' ' })
        .collect::<String>();
    let mut w = t.split_whitespace().collect::<Vec<_>>();
    w.sort();
    w.join(" ")
}
fn collect_titles(v: &Value, out: &mut BTreeSet<String>) {
    match v {
        Value::Object(o) => {
            for (k, x) in o {
                if [
                    "title",
                    "source_title",
                    "expected_title",
                    "catalog_title",
                    "item_title",
                    "canonical_work_title",
                ]
                .contains(&k.as_str())
                {
                    if let Some(t) = x.as_str() {
                        out.extend(title_keys(t));
                    }
                } else if ["work_aliases", "title_aliases"].contains(&k.as_str()) {
                    if let Some(a) = x.as_array() {
                        for t in a.iter().filter_map(Value::as_str) {
                            out.extend(title_keys(t));
                        }
                    }
                } else if x.is_object() || x.is_array() {
                    collect_titles(x, out);
                }
            }
        }
        Value::Array(a) => {
            for x in a {
                collect_titles(x, out);
            }
        }
        _ => {}
    }
}
fn named_authors(v: &Value) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    if let Some(a) = v["source"]["author_ids"].as_array() {
        out.extend(a.iter().filter_map(Value::as_str).map(author_key));
    }
    // This never assigns a synopsis writer from the summarized book's author.
    for k in [
        "author",
        "author_key",
        "catalog_author",
        "book_author",
        "underlying_work_author",
    ] {
        for obj in [v, &v["source"], &v["metadata"]] {
            if let Some(s) = obj[k].as_str() {
                out.insert(author_key(s));
            }
        }
    }
    out
}
#[derive(Default)]
struct Protected {
    families: BTreeSet<String>,
    titles: BTreeSet<String>,
    pg_titles: BTreeSet<String>,
    authors: BTreeSet<String>,
    ids: BTreeSet<String>,
    test_ids: BTreeSet<String>,
    exact: BTreeMap<String, String>,
    normalized: BTreeMap<String, String>,
    splits: BTreeMap<String, String>,
}
impl Protected {
    fn work(&mut self, w: &Value) {
        collect_titles(w, &mut self.titles);
        for k in ["underlying_work_key", "source_group", "group_id"] {
            if let Some(s) = w[k].as_str() {
                self.families.insert(s.into());
            }
        }
        if w["enrollment"] == "withheld" {
            for k in ["author", "author_key", "catalog_author"] {
                if let Some(s) = w[k].as_str() {
                    self.authors.insert(author_key(s));
                }
            }
        }
    }
    fn metadata_reasons(&self, c: &Value) -> Vec<String> {
        let mut r = Vec::new();
        if c["source_group"]
            .as_str()
            .is_some_and(|s| self.families.contains(s))
        {
            r.push("reserved_source_family".into());
        }
        let mut titles = BTreeSet::new();
        collect_titles(c, &mut titles);
        if !titles.is_disjoint(&self.pg_titles) {
            r.push("reserved_pg32_underlying_book".into());
        }
        if !titles.is_disjoint(&self.titles) {
            r.push("reserved_underlying_work_title".into());
        }
        if !named_authors(c).is_disjoint(&self.authors) {
            r.push("reserved_withheld_author".into());
        }
        r
    }
    fn cumulative(&mut self, r: &Value) -> Result<()> {
        let id = str_at(r, "id")?;
        let part = str_at(r, "frozen_split")?;
        ensure!(
            PARTS.contains(&part) && !self.test_ids.contains(id),
            "Test/unknown cumulative record"
        );
        ensure!(self.ids.insert(id.into()), "duplicate cumulative ID");
        let group = str_at(r, "source_group")?;
        if let Some(old) = self.splits.insert(group.into(), part.into()) {
            ensure!(old == part, "cross-split source family");
        }
        let t = str_at(r, "text")?;
        let exact = sha(t.as_bytes());
        ensure!(r["text_sha256"] == exact, "cumulative text digest");
        for (map, key) in [
            (&mut self.exact, exact),
            (&mut self.normalized, norm_sha(t)),
        ] {
            if let Some(old) = map.insert(key, part.into()) {
                ensure!(old == part, "cross-split duplicate text");
            }
        }
        self.families.insert(group.into());
        collect_titles(&r["source"], &mut self.titles);
        Ok(())
    }
}

#[derive(Deserialize)]
struct Header {
    id: String,
    source_group: String,
    frozen_split: String,
    purpose: String,
}
fn authorize_header(
    h: &Header,
    ids: &BTreeSet<String>,
    test: &BTreeSet<String>,
    splits: &Value,
    spec: &Value,
) -> Result<()> {
    ensure!(
        ids.contains(&h.id) && !test.contains(&h.id),
        "unexpected/reserved Test ID"
    );
    ensure!(
        PARTS.contains(&h.frozen_split.as_str())
            && array(spec, "allowed_splits")?
                .iter()
                .any(|p| p == &h.frozen_split),
        "Test/unknown/wrong cumulative split"
    );
    ensure!(
        splits[&h.source_group] == h.frozen_split,
        "cumulative frozen family split"
    );
    ensure!(
        ["query", "reference"].contains(&h.purpose.as_str()),
        "unknown purpose"
    );
    if spec["references_only"] == true {
        ensure!(
            h.purpose == "reference" && h.frozen_split != "train",
            "partition reference scope"
        );
    } else {
        ensure!(
            h.purpose == "query" || h.frozen_split == "train",
            "non-Train references require separate population"
        );
    }
    Ok(())
}
fn numeric_binding(n: &Value, m: &Value, manifest_sha: &str) -> Result<()> {
    ensure!(
        n["schema"] == "slop_ninja_shared_a_numeric_contract_v1" && n["final_test_opened"] == false,
        "frozen numeric contract scope"
    );
    ensure!(
        n["records_sha256"] == m["records_sha256"]
            && n["dataset_manifest_sha256"] == manifest_sha
            && n["split_assignments"] == m["split_assignments"],
        "numeric/record dataset binding"
    );
    Ok(())
}
fn population(
    spec: &Value,
    m: &Value,
    bytes: &[u8],
    p: &mut Protected,
) -> Result<BTreeSet<String>> {
    ensure!(
        m["schema"] == "slop_ninja_a_supervised_dataset_v1" && m["test_text_opened"] == false,
        "non-Test population manifest"
    );
    let ids = strings(m, "non_test_ids")?;
    let test = strings(m, "test_ids")?;
    ensure!(
        ids.len() as u64 == counter(spec, "count")? && ids.is_disjoint(&test),
        "population roster/count"
    );
    ensure!(
        m["records_sha256"] == sha(bytes) && spec["records"]["sha256"] == m["records_sha256"],
        "population records binding"
    );
    ensure!(
        m["reference_transform_only"].as_bool().unwrap_or(false)
            == (spec["references_only"] == true),
        "reference manifest route"
    );
    p.test_ids.extend(test.iter().cloned());
    ensure!(
        p.ids.is_disjoint(&p.test_ids),
        "previous cumulative ID reserved as Test"
    );
    let mut seen = BTreeSet::new();
    let mut families = BTreeSet::new();
    for line in std::str::from_utf8(bytes)?.lines() {
        // Deserialize only routing metadata before interpreting prose.
        let h: Header = serde_json::from_str(line)?;
        authorize_header(&h, &ids, &p.test_ids, &m["split_assignments"], spec)?;
        ensure!(seen.insert(h.id), "duplicate population ID");
        families.insert(h.source_group);
        let r: Value = serde_json::from_str(line)?;
        p.cumulative(&r)?;
    }
    ensure!(seen == ids, "incomplete cumulative population");
    let declared = m["split_assignments"]
        .as_object()
        .context("split assignments")?
        .iter()
        .filter(|(_, v)| v.as_str().is_some_and(|s| PARTS.contains(&s)))
        .map(|(k, _)| k.clone())
        .collect::<BTreeSet<_>>();
    ensure!(
        families == declared,
        "cumulative family roster differs from manifest"
    );
    Ok(families)
}

fn historical_date(r: &Value) -> Option<(&str, &str)> {
    let (raw, basis) = if let Some(s) = r["source"]["published_at"]
        .as_str()
        .filter(|s| !s.is_empty())
    {
        (s, "source.published_at")
    } else {
        (
            r["evidence_notes"]
                .as_str()?
                .split_once("Historical revision timestamp: ")?
                .1,
            "original_evidence_notes_historical_revision_timestamp",
        )
    };
    let d = raw.get(..10)?;
    let b = d.as_bytes();
    if b[4] != b'-'
        || b[7] != b'-'
        || !b
            .iter()
            .enumerate()
            .all(|(i, c)| i == 4 || i == 7 || c.is_ascii_digit())
    {
        return None;
    }
    let y = d[..4].parse::<u32>().ok()?;
    let m = d[5..7].parse::<usize>().ok()?;
    let day = d[8..].parse::<u32>().ok()?;
    let leap = y % 4 == 0 && (y % 100 != 0 || y % 400 == 0);
    let days = [
        31,
        if leap { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    if y == 0 || !(1..=12).contains(&m) || day == 0 || day > days[m - 1] {
        return None;
    }
    Some((d, basis))
}
fn root_reasons(r: &Value) -> Vec<String> {
    let mut reasons = Vec::new();
    if r["split"] != "train" {
        reasons.push("not_original_train".into());
    }
    if r["origin"] != "human_only"
        || r["evidence"] != "historical_proxy"
        || !r["parent_id"].is_null()
        || !r["generation"].is_null()
    {
        reasons.push("not_original_historical_human_root".into());
    }
    let collection = r["source"]["collection"].as_str().unwrap_or("");
    if !COLLECTIONS.contains(&collection) {
        reasons.push("unknown_collection".into());
    }
    if [
        "commercial_training",
        "model_release",
        "external_evaluation",
        "redistribute_text",
    ]
    .iter()
    .any(|k| r["rights"][k] != true)
    {
        reasons.push("rights_not_declared".into());
    }
    if ["license", "evidence_url", "attribution"]
        .iter()
        .any(|k| str_at(&r["rights"], k).is_err())
        || digest_at(&r["rights"], "evidence_sha256").is_err()
    {
        reasons.push("missing_rights_evidence".into());
    }
    if ["url", "version", "extraction"]
        .iter()
        .any(|k| str_at(&r["source"], k).is_err())
        || digest_at(&r["source"], "raw_sha256").is_err()
        || str_at(r, "source_group").is_err()
    {
        reasons.push("missing_source_provenance".into());
    }
    if historical_date(r).is_none_or(|(d, _)| d >= "2022-11-01") {
        reasons.push("missing_or_nonhistorical_publication".into());
    }
    if ["cmu-books-historical", "hc3-wiki-historical"].contains(&collection) {
        if str_at(&r["rights"]["share_alike"], "source_title").is_err() {
            reasons.push("missing_required_underlying_work_title".into());
        }
        if !r["source_group"].as_str().is_some_and(|s| {
            s.strip_prefix("wikipedia:page:")
                .is_some_and(|p| !p.is_empty() && p.bytes().all(|c| c.is_ascii_digit()))
        }) {
            reasons.push("missing_required_wikipedia_page_identity".into());
        }
        for k in [
            "article_url",
            "history_url",
            "source_license",
            "source_license_url",
            "policy",
        ] {
            if str_at(&r["rights"]["share_alike"], k).is_err() {
                reasons.push(format!("missing_share_alike_{k}"));
            }
        }
        if array(&r["rights"]["share_alike"], "notices").is_err() {
            reasons.push("missing_share_alike_notices".into());
        }
    }
    reasons
}
fn bind_original(r: &Value, meta: &Value) -> Result<()> {
    for (k, v) in meta.as_object().context("credit record")? {
        let mut actual = r
            .get(k)
            .with_context(|| format!("missing original {k}"))?
            .clone();
        // The pinned public credit projection omits the local raw-file locator.
        // This is the only permitted source projection: all other fields compare
        // exactly. The entire original shard is byte-pinned and the complete
        // original source, including raw_path, is retained in source.record.
        if k == "source" && v.get("raw_path").is_none() {
            actual
                .as_object_mut()
                .context("original source object")?
                .remove("raw_path");
        }
        ensure!(
            k != "text" && actual == *v,
            "original/credit metadata mismatch in {k}; actual object fields {:?}; projected fields {:?}",
            r[k].as_object().map(|o| o.keys().collect::<Vec<_>>()),
            v.as_object().map(|o| o.keys().collect::<Vec<_>>())
        );
    }
    Ok(())
}
fn bind_candidate(c: &Value, m: &Value) -> Result<()> {
    ensure!(
        c["root_id"] == m["id"]
            && c["source_group"] == m["source_group"]
            && c["text_sha256"] == m["text_sha256"]
            && c["source"] == m["source"]
            && c["rights"] == m["rights"],
        "audited candidate/source credit binding"
    );
    Ok(())
}

struct Candidate {
    request: Value,
    record: Value,
    line_sha: String,
    rank: String,
}
fn select(mut candidates: Vec<Candidate>, p: &Protected) -> Result<(Vec<Candidate>, Vec<Value>)> {
    candidates.sort_by(|a, b| {
        a.rank
            .cmp(&b.rank)
            .then_with(|| {
                a.record["source_group"]
                    .as_str()
                    .cmp(&b.record["source_group"].as_str())
            })
            .then_with(|| a.record["id"].as_str().cmp(&b.record["id"].as_str()))
    });
    let mut families = p.families.clone();
    let mut exact = p.exact.keys().cloned().collect::<BTreeSet<_>>();
    let mut normalized = p.normalized.keys().cloned().collect::<BTreeSet<_>>();
    let mut titles = p.titles.clone();
    let mut selected = Vec::new();
    let mut audit = Vec::new();
    for c in candidates {
        let r = &c.record;
        let group = str_at(r, "source_group")?;
        let mut reasons = root_reasons(r);
        reasons.extend(p.metadata_reasons(&c.request));
        reasons.extend(p.metadata_reasons(r));
        let mut work_titles = BTreeSet::new();
        collect_titles(&c.request, &mut work_titles);
        if families.contains(group) {
            reasons.push("duplicate_or_reserved_family".into());
        }
        if !work_titles.is_disjoint(&titles) {
            reasons.push("duplicate_or_reserved_work_alias".into());
        }
        let t = str_at(r, "text")?;
        let hash = sha(t.as_bytes());
        let n = t.split_whitespace().count();
        let norm = norm_sha(t);
        if r["text_sha256"] != hash {
            reasons.push("original_text_digest_mismatch".into());
        }
        if !(150..=450).contains(&n) {
            reasons.push("word_count_outside_150_450".into());
        }
        let prior = &c.request["prior_selection_decision"];
        if prior["words"] != n
            || prior["text_sha256"] != hash
            || prior["normalized_text_sha256"] != norm
        {
            reasons.push("prior_candidate_text_audit_mismatch".into());
        }
        if exact.contains(&hash) {
            reasons.push("duplicate_exact_text".into());
        }
        if normalized.contains(&norm) {
            reasons.push("duplicate_normalized_text".into());
        }
        reasons.sort();
        reasons.dedup();
        audit.push(json!({"original_id":r["id"],"source_group":group,"collection":r["source"]["collection"],"title":c.request["title"],
            "rank_sha256":c.rank,"words":n,"text_sha256":hash,"normalized_text_sha256":norm,"original_jsonl_line_sha256":c.line_sha,
            "selected":reasons.is_empty(),"reasons":reasons,"request_sha256":sha(&serde_json::to_vec(&c.request)?)}));
        if reasons.is_empty() {
            families.insert(group.into());
            exact.insert(hash);
            normalized.insert(norm);
            titles.extend(work_titles);
            selected.push(c);
        }
    }
    Ok((selected, audit))
}
fn reconcile(requests: &[Value], audit: &[Value], selected: usize) -> Result<()> {
    let requested = requests
        .iter()
        .map(|r| str_at(r, "root_id").map(str::to_owned))
        .collect::<Result<BTreeSet<_>>>()?;
    let mut accounted = BTreeSet::new();
    let mut admitted = 0;
    for a in audit {
        ensure!(
            accounted.insert(str_at(a, "original_id")?.to_owned()),
            "duplicate audit decision"
        );
        let reasons = array(a, "reasons")?;
        ensure!(
            a["selected"] == reasons.is_empty(),
            "inconsistent audit decision"
        );
        admitted += usize::from(reasons.is_empty());
    }
    ensure!(
        requested.len() == requests.len() && requested == accounted && admitted == selected,
        "request/admission/exclusion reconciliation"
    );
    Ok(())
}
fn supervised(
    c: &Candidate,
    batch: &str,
    plan_sha: &str,
    protocol_sha: &str,
    train_sha: &str,
    manifest_sha: &str,
) -> Result<Value> {
    let r = &c.record;
    let t = str_at(r, "text")?;
    let mut history = r.clone();
    history
        .as_object_mut()
        .context("record object")?
        .remove("text");
    let id = sha(format!(
        "slop_ninja_retained_train_human_v1\0{batch}\0{}",
        str_at(r, "id")?
    )
    .as_bytes());
    let result = json!({"schema":"slop_ninja_a_supervised_record_v1","id":format!("train-retained-human-{id}"),
        "text":t,"text_sha256":r["text_sha256"],"source_group":r["source_group"],"frozen_split":"train","purpose":"query",
        "origin":"human_only","evidence":"historical_proxy","documented_human_only":false,
        "human_identity":null,"human_role_present":true,"ai_identity":null,"ai_role_present":false,
        "share_target":0.0,"share_evidence":{"basis":"historical_human_proxy_endpoint","not_documented_contemporary_workflow":true},
        "exposed_to_grammar":null,"grammar_exposure":"unknown",
        "metadata":{"source_collection":r["source"]["collection"],"original_record_id":r["id"],"word_count":t.split_whitespace().count(),
            "selection_rank_sha256":c.rank,"v6_upstream_exposure":"original_train","historical_date":historical_date(r).map(|(d,_)|d),
            "historical_date_basis":historical_date(r).map(|(_,basis)|basis),"source_batch":batch,"admission_plan_sha256":plan_sha,"admission_protocol_sha256":protocol_sha},
        "source":{"record":history,"original_train_sha256":train_sha,"source_manifest_sha256":manifest_sha,"original_jsonl_line_sha256":c.line_sha,
            "byte_span":[0,t.len()],"extraction":"unchanged_whole_original_v6_train_human_record_v1","original_text_sha256":r["text_sha256"]},
        "rights":r["rights"],"parent_id":null});
    supervision::Supervision::parse(&result)?;
    Ok(result)
}

fn run(plan_path: &Path, out: &Path) -> Result<Value> {
    ensure!(!out.exists(), "new output directory required");
    let pb = read(plan_path)?;
    let plan: Value = serde_json::from_slice(&pb)?;
    let plan_sha = sha(&pb);
    ensure!(plan["schema"] == PLAN_SCHEMA, "admission plan schema");
    let batch = str_at(&plan, "batch_id")?;
    let files = &plan["files"];
    let mut input = Inputs {
        base: plan_path.parent().context("plan parent")?.into(),
        pins: BTreeMap::new(),
    };
    input.pins.insert(
        plan_path.display().to_string(),
        json!({"sha256":plan_sha,"bytes":pb.len()}),
    );
    let protocol = input.pinned(&files["protocol"])?;
    ensure!(
        std::str::from_utf8(&protocol)?
            .starts_with("Slop Ninja retained Train expansion admission, version 1"),
        "protocol version"
    );
    let protocol_sha = sha(&protocol);
    // All metadata and scope gates precede corpus reads.
    let audit_manifest = input.json(&files["candidate_audit"])?;
    let validation = input.json(&files["candidate_validation"])?;
    ensure!(
        audit_manifest["schema"] == "slop_ninja_next_train_data_candidate_plan_v1"
            && validation["schema"] == "slop_ninja_next_train_metadata_validation_v1",
        "candidate audit schemas"
    );
    ensure!(
        audit_manifest["first_batch"]["candidate_family_ids"] == plan["requested"]
            && validation["standard_prior_eligible_family_count"] == plan["requested"],
        "requested/audited count"
    );
    ensure!(
        array(&audit_manifest, "artifacts")?
            .iter()
            .any(|v| v["file"] == "retained-v6-standard-candidates.jsonl"
                && v["sha256"] == files["candidates"]["sha256"]),
        "candidate file absent from frozen audit"
    );
    let request_bytes = input.pinned(&files["candidates"])?;
    let requests = parse_rows(&request_bytes)?;
    ensure!(
        requests.len() as u64 == counter(&plan, "requested")?,
        "request count"
    );
    let mut request_map = BTreeMap::new();
    let mut request_families = BTreeSet::new();
    for r in &requests {
        let prior = &r["prior_selection_decision"];
        ensure!(
            r["length_lane"] == "existing_150_450_quota_only"
                && prior["selected"] == false
                && prior["reasons"] == json!(["collection_quota"]),
            "nonstandard or unqualified requested candidate"
        );
        ensure!(
            request_map
                .insert(str_at(r, "root_id")?.to_owned(), r)
                .is_none()
                && request_families.insert(str_at(r, "source_group")?.to_owned()),
            "duplicate requested identity/family"
        );
    }
    let reservations = input.json(&files["reservations"])?;
    let prior = input.json(&files["prior_reservations"])?;
    ensure!(
        reservations["schema"] == "slop_ninja_next_train_reservations_metadata_v1"
            && prior["schema"] == "slop_ninja_prior_pilot_reservations_metadata_v1",
        "reservation schemas"
    );
    ensure!(
        array(&reservations, "fresh_cohort_works")?.len() as u64
            == counter(&plan, "current_roster_works")?
            && array(&prior, "works")?.len() as u64 == counter(&plan, "prior_roster_works")?,
        "reserved roster counts"
    );
    let mut protected = Protected::default();
    for k in [
        "fresh_cohort_works",
        "current_augmentation_96",
        "original_v6_development_calibration_families",
        "matched_plos_reserved_heldouts",
    ] {
        for w in array(&reservations, k)? {
            protected.work(w);
        }
    }
    // The all-partition Wikipedia title list is NOT blanket-blocked here: it
    // contains these original Train candidates. It served external PG scouting.
    for w in array(&prior, "works")? {
        protected.work(w);
    }
    for (v, k) in [
        (&reservations, "whole_author_withheld"),
        (&prior, "withheld_authors"),
    ] {
        for s in strings(v, k)? {
            protected.authors.insert(author_key(&s));
        }
    }
    for w in array(&reservations, "prior_global_voices_reserved")? {
        protected.work(w);
    }
    let pg = parse_rows(&input.pinned(&files["pg_candidates"])?)?;
    ensure!(
        pg.len() as u64 == counter(&plan, "pg_reserved_items")?,
        "PG reserved count"
    );
    let mut pg_ids = BTreeSet::new();
    for w in &pg {
        ensure!(
            pg_ids.insert(counter(w, "pg_id")?),
            "duplicate PG reservation"
        );
        let mut keys = BTreeSet::new();
        collect_titles(w, &mut keys);
        ensure!(!keys.is_empty(), "missing PG work titles");
        protected.pg_titles.extend(keys);
    }
    let old_roots = input.json(&files["prior_roots_manifest"])?;
    let old_selection = input.json(&files["prior_selection"])?;
    ensure!(
        old_roots["schema"] == "slop_ninja_train_augmentation_roots_v1"
            && old_roots["complete"] == true
            && old_roots["test_text_opened"] == false
            && old_roots["selection_sha256"] == files["prior_selection"]["sha256"]
            && old_roots["protocol_sha256"] == old_selection["protocol_sha256"],
        "prior root selection binding"
    );
    let mut old_groups = BTreeSet::new();
    for r in array(&old_selection, "selected")? {
        ensure!(
            old_groups.insert(str_at(r, "source_group")?.to_owned()),
            "duplicate old root"
        );
        protected.work(r);
    }
    ensure!(
        old_groups.len() as u64 == counter(&old_roots, "records")?,
        "prior root count"
    );
    for k in [
        "original_train",
        "snapshot_manifest",
        "credits",
        "current_manifest",
        "references_manifest",
    ] {
        let pin = &files[k];
        ensure!(
            old_roots["input_files"][str_at(pin, "path")?]["sha256"] == pin["sha256"],
            "original selector input changed: {k}"
        );
    }
    let snapshot = input.json(&files["snapshot_manifest"])?;
    let credits = input.json(&files["credits"])?;
    ensure!(
        snapshot["schema"] == "slop_ninja_origin_shards_v1"
            && credits["schema"] == "slop_ninja_corpus_attribution_v2"
            && credits["provenance"]["test_partition_opened_by_extractor"] == false
            && credits["partitions"]["train"] == snapshot["partitions"]["train"],
        "original Train metadata binding"
    );
    ensure!(
        files["original_train"]["sha256"] == snapshot["partitions"]["train"]["sha256"],
        "original Train plan pin"
    );
    ensure!(
        Path::new(str_at(&files["original_train"], "path")?)
            .file_name()
            .is_some_and(|s| s == "train.jsonl"),
        "only original train.jsonl allowed"
    );
    let mut original_meta = BTreeMap::new();
    for r in array(&credits, "records")? {
        ensure!(
            r.get("text").is_none() && PARTS.contains(&str_at(r, "split")?),
            "credits contain prose/Test/unknown partition"
        );
        if r["split"] == "train" {
            ensure!(
                original_meta
                    .insert(str_at(r, "id")?.to_owned(), r)
                    .is_none(),
                "duplicate original metadata ID"
            );
        } else {
            protected.work(r);
        }
    }
    let numeric = input.json(&files["numeric_contract"])?;
    let mut pop_manifests = BTreeMap::new();
    let mut kinds = BTreeSet::new();
    for spec in array(&plan, "populations")? {
        let kind = str_at(spec, "kind")?;
        ensure!(
            ["current", "references", "augmentation"].contains(&kind)
                && kinds.insert(kind.to_owned()),
            "unknown/duplicate population kind"
        );
        let key = str_at(spec, "manifest_key")?;
        let m = input.json(&files[key])?;
        ensure!(
            m["test_text_opened"] == false && m["records_sha256"] == spec["records"]["sha256"],
            "closed non-Test population binding"
        );
        ensure!(
            strings(&m, "non_test_ids")?.len() as u64 == counter(spec, "count")?,
            "pinned cumulative population count"
        );
        if kind == "current" {
            numeric_binding(&numeric, &m, digest_at(&files[key], "sha256")?)?;
        }
        if kind == "augmentation" {
            ensure!(
                m["train_extension_only"] == true && m["complete"] == true,
                "incomplete/non-Train prior augmentation"
            );
        }
        pop_manifests.insert(kind.to_owned(), m);
    }
    ensure!(
        kinds == BTreeSet::from(["current".into(), "references".into(), "augmentation".into()]),
        "all cumulative populations required"
    );
    for spec in array(&plan, "populations")? {
        let kind = str_at(spec, "kind")?;
        let b = input.pinned(&spec["records"])?;
        let families = population(spec, &pop_manifests[kind], &b, &mut protected)?;
        if kind == "augmentation" {
            ensure!(
                families == old_groups,
                "prior augmentation/selected-root families differ"
            );
        }
    }
    let train_bytes = input.pinned(&files["original_train"])?;
    let train_sha = sha(&train_bytes);
    let mut seen = BTreeSet::new();
    let mut candidates = Vec::new();
    let mut decisions = Vec::new();
    let mut found_requests = BTreeSet::new();
    for line in std::str::from_utf8(&train_bytes)?.lines() {
        let r: Value = serde_json::from_str(line)?;
        ensure!(
            r["split"] == "train",
            "non-Train row in original Train shard"
        );
        let id = str_at(&r, "id")?.to_owned();
        ensure!(seen.insert(id.clone()), "duplicate original ID");
        let meta = original_meta
            .get(&id)
            .context("original ID absent from pinned credits")?;
        // Non-requested AI records have a text-free generation projection in
        // credits, so their raw prompt objects are intentionally not compared.
        // The entire original shard remains byte-pinned. Requested human roots
        // below must match every original projected metadata field exactly.
        for key in ["id", "source_group", "text_sha256", "origin", "evidence"] {
            ensure!(r[key] == meta[key], "original header mismatch: {key}");
        }
        if let Some(request) = request_map.get(&id) {
            bind_original(&r, meta)?;
            bind_candidate(request, meta)?;
            found_requests.insert(id.clone());
            candidates.push(Candidate {
                request: (*request).clone(),
                rank: rank(str_at(&r, "source_group")?),
                record: r,
                line_sha: sha(line.as_bytes()),
            });
        }
    }
    ensure!(
        seen.len() == original_meta.len()
            && seen.len() as u64 == counter(&snapshot["partitions"]["train"], "rows")?,
        "incomplete original Train roster"
    );
    for (id, c) in &request_map {
        if !found_requests.contains(id) {
            decisions.push(json!({"original_id":id,"source_group":c["source_group"],"collection":c["source"]["collection"],"selected":false,"reasons":["missing_original_train_record"]}));
        }
    }
    let (selected, selected_audit) = select(candidates, &protected)?;
    decisions.extend(selected_audit);
    decisions.sort_by(|a, b| a["original_id"].as_str().cmp(&b["original_id"].as_str()));
    reconcile(&requests, &decisions, selected.len())?;
    let humans = selected
        .iter()
        .map(|c| {
            supervised(
                c,
                batch,
                &plan_sha,
                &protocol_sha,
                &train_sha,
                digest_at(&files["snapshot_manifest"], "sha256")?,
            )
        })
        .collect::<Result<Vec<_>>>()?;
    let ids = humans
        .iter()
        .map(|r| str_at(r, "id").map(str::to_owned))
        .collect::<Result<BTreeSet<_>>>()?;
    ensure!(
        ids.len() == humans.len()
            && ids.is_disjoint(&protected.ids)
            && ids.is_disjoint(&protected.test_ids),
        "new root ID collision"
    );
    let hb = encode_rows(&humans)?;
    let ab = encode_rows(&decisions)?;
    let exclusions = decisions
        .iter()
        .filter(|d| d["selected"] == false)
        .cloned()
        .collect::<Vec<_>>();
    let eb = encode_rows(&exclusions)?;
    let counts=COLLECTIONS.iter().map(|c|json!({"collection":c,"requested":requests.iter().filter(|r|r["source"]["collection"]==*c).count(),"admitted":humans.iter().filter(|r|r["metadata"]["source_collection"]==*c).count(),"excluded":exclusions.iter().filter(|r|r["collection"]==*c).count()})).collect::<Vec<_>>();
    let selection = json!({"schema":"slop_ninja_retained_train_selection_v1","batch_id":batch,"protocol_sha256":protocol_sha,"plan_sha256":plan_sha,"collections":counts,
        "rank_domain":"slop_ninja_train_augmentation_roots_v1\\0","selected":decisions.iter().filter(|r|r["selected"]==true).collect::<Vec<_>>()});
    let sb = serde_json::to_vec_pretty(&selection)?;
    let mut reason_counts = BTreeMap::<String, usize>::new();
    for d in &exclusions {
        for reason in array(d, "reasons")? {
            *reason_counts
                .entry(reason.as_str().context("reason")?.into())
                .or_default() += 1;
        }
    }
    let manifest = json!({"schema":"slop_ninja_retained_train_roots_v1","batch_id":batch,"requested":requests.len(),"records":humans.len(),"excluded":exclusions.len(),"source_families":humans.len(),
        "complete":true,"completion_means":"every pinned request accounted for; exclusions retained; no replacement","records_file":"humans.jsonl","records_sha256":sha(&hb),"requests_sha256":sha(&request_bytes),
        "selection_sha256":sha(&sb),"exclusion_audit_sha256":sha(&ab),"exclusions_sha256":sha(&eb),"collections":counts,"reason_counts":reason_counts,"input_files":input.pins,
        "protocol_sha256":protocol_sha,"plan_sha256":plan_sha,"implementation_sha256":sha(include_bytes!("admit_train_expansion.rs")),"supervision_implementation_sha256":sha(include_bytes!("prepare_supervised_forest/supervision.rs")),
        "original_train_rows":seen.len(),"protected_cumulative_records":protected.ids.len(),"protected_cumulative_families":protected.splits.len(),"reserved_pg_items":pg.len(),
        "non_test_ids":ids,"test_ids":[],"test_text_opened":false,"model_calls":0,"downloads":0,"train_only":true,"normalization_fitted":false,
        "limits":["historical proxies, not documented human-only production; unknown writer identities masked","original V6 Train exposure and unknown CFG exposure","work aliases/subject-author coverage limited to retained metadata","original Test separation relies on original Train split audit; no Test prose read","word count is not tokenizer qualification","manifest uses admission schema; older quota-only assembler must not silently reinterpret exclusions"]});
    let report = format!(
        "Slop Ninja retained Train admission completed.\n\nRequested: {}. Admitted independent source families: {}. Excluded: {}. No replacements.\n\nCollections: {}\n\nExclusion reason counts (may overlap): {}\n\nAll exact input bindings are in manifest.json. Every request is retained in requests.jsonl and every decision in exclusion-audit.jsonl; exclusions.jsonl is the excluded subset. Original rows/rights/history and whole-record bytes are preserved in humans.jsonl. Individual writer identities remain null; historical zero-share proxy evidence is explicit.\n\nProtocol SHA-256: {}\nPlan SHA-256: {}\nHumans SHA-256: {}\n\nChecked {} original Train rows and {} cumulative non-Test records. Reserved {} PG items. No Test prose, generation, downloads or model calls. Existing datasets remain unchanged. Existing generator consumes the unchanged supervised per-row shape; the new manifest explicitly accounts for exclusions and is not the old quota-only roots manifest. Alias coverage remains limited to retained metadata; later tokenizer qualification is required without truncation.\n",
        requests.len(),
        humans.len(),
        exclusions.len(),
        serde_json::to_string(&counts)?,
        serde_json::to_string(&reason_counts)?,
        protocol_sha,
        plan_sha,
        sha(&hb),
        seen.len(),
        protected.ids.len(),
        pg.len()
    );
    fs::create_dir(out)?;
    put(&out.join("requests.jsonl"), &request_bytes)?;
    put(&out.join("humans.jsonl"), &hb)?;
    put(&out.join("selection.json"), &sb)?;
    put(&out.join("exclusion-audit.jsonl"), &ab)?;
    put(&out.join("exclusions.jsonl"), &eb)?;
    put(&out.join("ADMISSION_PROTOCOL.md"), &protocol)?;
    put(&out.join("admission-plan.json"), &pb)?;
    put(&out.join("REPORT.md"), report.as_bytes())?;
    let mut sums = String::new();
    for (path, pin) in &input.pins {
        sums.push_str(&format!("{}  {path}\n", digest_at(pin, "sha256")?));
    }
    put(&out.join("input-checksums.sha256"), sums.as_bytes())?;
    // Manifest is the completion marker and is written only after all outputs.
    put(
        &out.join("manifest.json"),
        &serde_json::to_vec_pretty(&manifest)?,
    )?;
    File::open(out)?.sync_all()?;
    Ok(
        json!({"requested":requests.len(),"admitted":humans.len(),"excluded":exclusions.len(),"records_sha256":sha(&hb),"collections":counts}),
    )
}
fn main() -> Result<()> {
    let a = std::env::args().skip(1).collect::<Vec<_>>();
    ensure!(
        a.len() == 2,
        "admit_train_expansion ADMISSION_PLAN.json NEW_OUT"
    );
    println!("{}", run(Path::new(&a[0]), Path::new(&a[1]))?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn candidate(id: &str, group: &str, word: &str) -> Candidate {
        let t = std::iter::repeat_n(word, 180).collect::<Vec<_>>().join(" ");
        let r = json!({"id":id,"source_group":group,"split":"train","origin":"human_only","evidence":"historical_proxy","parent_id":null,"generation":null,
            "text":t,"text_sha256":sha(t.as_bytes()),"source":{"collection":"plos_one","published_at":"2018-02-28","url":"https://example.invalid/article","version":"synthetic-v1","raw_sha256":"a".repeat(64),"extraction":"unchanged","author_ids":[]},
            "rights":{"license":"CC-BY-4.0","evidence_url":"https://example.invalid/license","evidence_sha256":"b".repeat(64),"attribution":"Synthetic test only","commercial_training":true,"model_release":true,"external_evaluation":true,"redistribute_text":true}});
        let request = json!({"root_id":id,"source_group":group,"title":format!("Synthetic {id}"),"text_sha256":r["text_sha256"],"source":r["source"],"rights":r["rights"],
            "prior_selection_decision":{"words":180,"text_sha256":r["text_sha256"],"normalized_text_sha256":norm_sha(&t)}});
        Candidate {
            request,
            record: r,
            line_sha: "c".repeat(64),
            rank: rank(group),
        }
    }
    fn supervised_fixture(id: &str, group: &str, part: &str, text: &str) -> Value {
        json!({"id":id,"source_group":group,"frozen_split":part,"purpose":"query","text":text,"text_sha256":sha(text.as_bytes())})
    }
    #[test]
    fn pg_book_and_prior_family_aliases_are_reserved() {
        let mut p = Protected::default();
        p.pg_titles
            .extend(title_keys("The Duke's Children: A Novel"));
        p.families.insert("old-family".into());
        let mut c = candidate("one", "different-page", "alpha");
        c.request["title"] = json!("Duke’s Children (novel)");
        let old = candidate("two", "old-family", "beta");
        let (selected, audit) = select(vec![c, old], &p).unwrap();
        assert!(selected.is_empty());
        assert!(audit.iter().any(|r| {
            r["reasons"]
                .as_array()
                .unwrap()
                .contains(&json!("reserved_pg32_underlying_book"))
        }));
        assert!(audit.iter().any(|r| {
            r["reasons"]
                .as_array()
                .unwrap()
                .contains(&json!("reserved_source_family"))
        }));
        p.authors.insert(author_key("Collins, Wilkie"));
        let mut r = candidate("three", "new-page", "gamma").record;
        r["source"]["author_ids"] = json!(["Wilkie Collins"]);
        assert!(
            p.metadata_reasons(&r)
                .contains(&"reserved_withheld_author".into())
        );
    }
    #[test]
    fn cumulative_and_candidate_duplicates_use_exact_and_whitespace_hashes() {
        let mut p = Protected::default();
        let c = candidate("one", "family-one", "alpha");
        let t = c.record["text"].as_str().unwrap();
        p.cumulative(&supervised_fixture(
            "existing",
            "existing-family",
            "train",
            &t.replace(' ', " \n\t"),
        ))
        .unwrap();
        let (selected, audit) = select(vec![c], &p).unwrap();
        assert!(selected.is_empty());
        assert!(
            audit[0]["reasons"]
                .as_array()
                .unwrap()
                .contains(&json!("duplicate_normalized_text"))
        );
        assert!(
            !audit[0]["reasons"]
                .as_array()
                .unwrap()
                .contains(&json!("duplicate_exact_text"))
        );
        let (selected, audit) = select(
            vec![
                candidate("one", "f1", "same"),
                candidate("two", "f2", "same"),
            ],
            &Protected::default(),
        )
        .unwrap();
        assert_eq!(selected.len(), 1);
        assert_eq!(audit.iter().filter(|r| r["selected"] == false).count(), 1);
    }
    #[test]
    fn rights_history_dates_and_metadata_gaps_are_exclusions() {
        let c = candidate("one", "family", "alpha");
        assert!(root_reasons(&c.record).is_empty());
        for key in [
            "commercial_training",
            "model_release",
            "external_evaluation",
            "redistribute_text",
        ] {
            let mut r = c.record.clone();
            r["rights"][key] = json!(false);
            assert!(root_reasons(&r).contains(&"rights_not_declared".into()));
        }
        let mut r = c.record.clone();
        r["parent_id"] = json!("model-parent");
        assert!(root_reasons(&r).contains(&"not_original_historical_human_root".into()));
        r = c.record.clone();
        r["source"]["published_at"] = Value::Null;
        r["evidence_notes"] = json!("Historical revision timestamp: 2018-01-02T00:00:00Z");
        assert_eq!(historical_date(&r).unwrap().0, "2018-01-02");
        for date in ["2018-02-29", "2022-11-01", "not-a-date", "0000-01-01"] {
            r["source"]["published_at"] = json!(date);
            assert!(root_reasons(&r).contains(&"missing_or_nonhistorical_publication".into()));
        }
        r = c.record.clone();
        r["source"]["collection"] = json!("cmu-books-historical");
        assert!(root_reasons(&r).contains(&"missing_required_underlying_work_title".into()));
        r = c.record.clone();
        r["rights"]["evidence_sha256"] = json!("not-a-digest");
        assert!(root_reasons(&r).contains(&"missing_rights_evidence".into()));
    }
    #[test]
    fn whole_record_length_boundaries_and_prior_hash_binding() {
        for (n, ok) in [(149, false), (150, true), (450, true), (451, false)] {
            let mut c = candidate("one", "family", "alpha");
            let t = std::iter::repeat_n("alpha", n)
                .collect::<Vec<_>>()
                .join(" ");
            c.record["text"] = json!(t);
            c.record["text_sha256"] = json!(sha(t.as_bytes()));
            c.request["prior_selection_decision"] = json!({"words":n,"text_sha256":sha(t.as_bytes()),"normalized_text_sha256":norm_sha(&t)});
            assert_eq!(
                !select(vec![c], &Protected::default()).unwrap().0.is_empty(),
                ok
            );
        }
        let mut c = candidate("one", "family", "alpha");
        c.request["prior_selection_decision"]["normalized_text_sha256"] = json!("0".repeat(64));
        let (selected, audit) = select(vec![c], &Protected::default()).unwrap();
        assert!(selected.is_empty());
        assert!(
            audit[0]["reasons"]
                .as_array()
                .unwrap()
                .contains(&json!("prior_candidate_text_audit_mismatch"))
        );
    }
    #[test]
    fn cross_split_corruption_and_test_are_rejected_before_prose() {
        let mut p = Protected::default();
        p.cumulative(&supervised_fixture("a", "family", "train", "one"))
            .unwrap();
        assert!(
            p.cumulative(&supervised_fixture("b", "family", "calibration", "two"))
                .is_err()
        );
        let mut p = Protected::default();
        p.cumulative(&supervised_fixture("a", "f1", "train", "one two"))
            .unwrap();
        assert!(
            p.cumulative(&supervised_fixture("b", "f2", "development", "one\n two"))
                .is_err()
        );
        let raw=json!({"id":"reserved","source_group":"test-work","frozen_split":"test","purpose":"query","text":{"not_prose":"must never inspect"}}).to_string();
        let h: Header = serde_json::from_str(&raw).unwrap();
        assert!(
            authorize_header(
                &h,
                &BTreeSet::from(["reserved".into()]),
                &BTreeSet::from(["reserved".into()]),
                &json!({"test-work":"test"}),
                &json!({"allowed_splits":["train","development","calibration"]})
            )
            .is_err()
        );
        let h = Header {
            id: "r".into(),
            source_group: "ref".into(),
            frozen_split: "calibration".into(),
            purpose: "reference".into(),
        };
        let ids = BTreeSet::from(["r".into()]);
        let splits = json!({"ref":"calibration"});
        assert!(
            authorize_header(
                &h,
                &ids,
                &BTreeSet::new(),
                &splits,
                &json!({"allowed_splits":["calibration"],"references_only":true})
            )
            .is_ok()
        );
        assert!(
            authorize_header(
                &h,
                &ids,
                &BTreeSet::new(),
                &splits,
                &json!({"allowed_splits":["calibration"],"references_only":false})
            )
            .is_err()
        );
    }
    #[test]
    fn frozen_numeric_and_full_original_metadata_are_bound() {
        let m = json!({"records_sha256":"a".repeat(64),"split_assignments":{"family":"train"}});
        let mut n = json!({"schema":"slop_ninja_shared_a_numeric_contract_v1","final_test_opened":false,"records_sha256":m["records_sha256"],"dataset_manifest_sha256":"b".repeat(64),"split_assignments":m["split_assignments"]});
        assert!(numeric_binding(&n, &m, &"b".repeat(64)).is_ok());
        n["split_assignments"]["family"] = json!("development");
        assert!(numeric_binding(&n, &m, &"b".repeat(64)).is_err());
        let mut c = candidate("one", "family", "alpha");
        c.record["source"]["raw_path"] = json!("synthetic/raw/source.json");
        let mut meta = c.record.clone();
        meta.as_object_mut().unwrap().remove("text");
        meta["source"].as_object_mut().unwrap().remove("raw_path");
        assert!(bind_original(&c.record, &meta).is_ok());
        assert!(bind_candidate(&c.request, &meta).is_ok());
        let mut changed = meta.clone();
        changed["source"]["raw_sha256"] = json!("f".repeat(64));
        assert!(bind_original(&c.record, &changed).is_err());
        changed = c.record.clone();
        changed["source"]["unexpected_field"] = json!("not an allowed projection");
        assert!(bind_original(&changed, &meta).is_err());
        meta["rights"]["attribution"] = json!("Changed credit");
        assert!(bind_original(&c.record, &meta).is_err());
        assert!(bind_candidate(&c.request, &meta).is_err());
    }
    #[test]
    fn every_request_is_accounted_without_replacement() {
        let a = candidate("a", "fa", "alpha");
        let b = candidate("b", "fb", "beta");
        let requests = vec![a.request.clone(), b.request.clone()];
        let mut p = Protected::default();
        p.families.insert("fb".into());
        let (selected, audit) = select(vec![a, b], &p).unwrap();
        assert_eq!(selected.len(), 1);
        reconcile(&requests, &audit, 1).unwrap();
        assert!(reconcile(&requests, &audit[..1], 1).is_err());
        assert!(reconcile(&requests, &audit, 2).is_err());
        let mut duplicated = audit.clone();
        duplicated.push(audit[0].clone());
        assert!(reconcile(&requests, &duplicated, 1).is_err());
    }
    #[test]
    fn output_preserves_bytes_rights_history_and_batch_identity() {
        let c = candidate("one", "family", "alpha");
        let row = supervised(
            &c,
            "batch1",
            &"a".repeat(64),
            &"b".repeat(64),
            &"c".repeat(64),
            &"d".repeat(64),
        )
        .unwrap();
        assert_eq!(row["text"], c.record["text"]);
        assert_eq!(row["rights"], c.record["rights"]);
        assert_eq!(row["source"]["record"]["source"], c.record["source"]);
        assert!(row["source"]["record"].get("text").is_none());
        assert_eq!(
            row["source"]["byte_span"],
            json!([0, c.record["text"].as_str().unwrap().len()])
        );
        assert!(row["human_identity"].is_null());
        assert_eq!(row["documented_human_only"], false);
        assert_eq!(row["metadata"]["source_batch"], "batch1");
        let other = supervised(
            &c,
            "batch2",
            &"a".repeat(64),
            &"b".repeat(64),
            &"c".repeat(64),
            &"d".repeat(64),
        )
        .unwrap();
        assert_ne!(row["id"], other["id"]);
    }
    #[test]
    fn pinned_reader_rejects_changed_inputs_and_never_overwrites() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("metadata.json");
        put(&p, b"{}").unwrap();
        assert!(put(&p, b"changed").is_err());
        let mut inputs = Inputs {
            base: dir.path().into(),
            pins: BTreeMap::new(),
        };
        let pin = json!({"path":"metadata.json","sha256":sha(b"{}")});
        assert_eq!(inputs.pinned(&pin).unwrap(), b"{}");
        fs::write(&p, b"{\"changed\":true}").unwrap();
        assert!(inputs.pinned(&pin).is_err());
    }
}
