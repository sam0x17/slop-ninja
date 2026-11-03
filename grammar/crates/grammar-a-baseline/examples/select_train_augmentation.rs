//! Select unchanged historical human Train roots; never open a Test shard.
//! V6_SNAPSHOT V6_CREDITS CURRENT_DATASET CURRENT_REFERENCES SELECTION_TSV PROTOCOL NEW_OUT
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Write,
    path::Path,
};

const COLLECTIONS: [&str; 4] = [
    "cmu-books-historical",
    "hc3-wiki-historical",
    "plos_one",
    "wikinews",
];
const QUOTA: usize = 24;
const DOMAIN: &str = "slop_ninja_train_augmentation_roots_v1\0";
const PROTOCOL_SHA: &str = "8467cb3f3997070a457109748a0fde38be5ebe6d59b7c0b5bd76c0c7df5250bc";
fn sha(b: &[u8]) -> String {
    hex::encode(Sha256::digest(b))
}
fn text<'a>(v: &'a Value, k: &str) -> Result<&'a str> {
    v[k].as_str().with_context(|| format!("missing {k}"))
}
fn read(p: &Path, pins: &mut BTreeMap<String, Value>) -> Result<Vec<u8>> {
    let bytes = fs::read(p).with_context(|| format!("read {}", p.display()))?;
    pins.insert(
        p.display().to_string(),
        json!({"sha256":sha(&bytes),"bytes":bytes.len()}),
    );
    Ok(bytes)
}
fn put(p: &Path, b: &[u8]) -> Result<()> {
    let mut f = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(p)?;
    f.write_all(b)?;
    f.sync_all()?;
    Ok(())
}
fn jsonl(rows: &[Value]) -> Result<Vec<u8>> {
    let mut b = Vec::new();
    for r in rows {
        serde_json::to_writer(&mut b, r)?;
        b.push(b'\n');
    }
    Ok(b)
}
fn normalized_sha(s: &str) -> String {
    sha(s
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .as_bytes())
}
fn words(s: &str) -> usize {
    s.split_whitespace().count()
}
fn title_key(s: &str) -> String {
    let s = s
        .to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { ' ' })
        .collect::<String>();
    let mut words = s.split_whitespace().collect::<Vec<_>>();
    if words
        .first()
        .is_some_and(|s| ["the", "a", "an"].contains(s))
    {
        words.remove(0);
    }
    words.join(" ")
}
fn title_keys(s: &str) -> BTreeSet<String> {
    [s, s.split([':', ';', '(']).next().unwrap_or(s)]
        .into_iter()
        .map(title_key)
        .filter(|s| !s.is_empty())
        .collect()
}
fn author_key(s: &str) -> String {
    let k = title_key(s);
    let mut w = k.split_whitespace().collect::<Vec<_>>();
    w.sort();
    w.join(" ")
}
#[derive(Default)]
struct Protected {
    families: BTreeSet<String>,
    titles: BTreeSet<String>,
    authors: BTreeSet<String>,
    exact: BTreeSet<String>,
    normalized: BTreeSet<String>,
}
impl Protected {
    fn roster(&mut self, b: &[u8]) -> Result<usize> {
        let s = std::str::from_utf8(b)?;
        let mut lines = s.lines();
        let head = lines
            .next()
            .context("TSV header")?
            .split('\t')
            .collect::<Vec<_>>();
        let mut n = 0;
        for line in lines {
            let row = line.split('\t').collect::<Vec<_>>();
            ensure!(row.len() == head.len(), "TSV columns");
            let get = |k: &str| -> Result<&str> {
                Ok(row[head
                    .iter()
                    .position(|x| *x == k)
                    .with_context(|| format!("TSV {k}"))?])
            };
            self.families.insert(get("underlying_work_key")?.into());
            self.titles.extend(title_keys(get("expected_title")?));
            if get("enrollment")? == "withheld" {
                for field in ["author_key", "author", "catalog_author"] {
                    self.authors.insert(author_key(get(field)?));
                }
            }
            n += 1;
        }
        Ok(n)
    }
    fn metadata_reasons(&self, r: &Value) -> Vec<String> {
        let mut reasons = Vec::new();
        if r["source_group"]
            .as_str()
            .is_some_and(|s| self.families.contains(s))
        {
            reasons.push("protected_source_family".into());
        }
        for title in [
            &r["rights"]["share_alike"]["source_title"],
            &r["source"]["title"],
            &r["title"],
        ] {
            if title
                .as_str()
                .is_some_and(|s| !title_keys(s).is_disjoint(&self.titles))
            {
                reasons.push("protected_title".into());
                break;
            }
        }
        if r["source"]["author_ids"].as_array().is_some_and(|a| {
            a.iter().any(|v| {
                v.as_str()
                    .is_some_and(|s| self.authors.contains(&author_key(s)))
            })
        }) {
            reasons.push("protected_withheld_author".into());
        }
        reasons
    }
    fn current(
        &mut self,
        dir: &Path,
        expected: usize,
        references: bool,
        pins: &mut BTreeMap<String, Value>,
    ) -> Result<()> {
        let mb = read(&dir.join("manifest.json"), pins)?;
        let m: Value = serde_json::from_slice(&mb)?;
        ensure!(
            m["test_text_opened"] == false,
            "current input must declare unopened Test"
        );
        let ids = m["non_test_ids"]
            .as_array()
            .context("non-Test IDs")?
            .iter()
            .map(|v| v.as_str().context("ID").map(str::to_owned))
            .collect::<Result<BTreeSet<_>>>()?;
        ensure!(ids.len() == expected, "current population count");
        // Only this declared non-Test records file is read, never test IDs/paths.
        let bytes = read(&dir.join("records.jsonl"), pins)?;
        ensure!(
            m["records_sha256"] == sha(&bytes),
            "current records changed"
        );
        let mut seen = BTreeSet::new();
        for line in std::str::from_utf8(&bytes)?.lines() {
            let r: Value = serde_json::from_str(line)?;
            ensure!(
                matches!(
                    r["frozen_split"].as_str(),
                    Some("train" | "development" | "calibration")
                ),
                "Test/unknown split forbidden"
            );
            ensure!(
                matches!(r["purpose"].as_str(), Some("query" | "reference")),
                "unknown purpose"
            );
            ensure!(
                !references || r["purpose"] == "reference",
                "reference expected"
            );
            let id = text(&r, "id")?;
            ensure!(
                ids.contains(id) && seen.insert(id.to_owned()),
                "unexpected/duplicate current ID"
            );
            let t = text(&r, "text")?;
            ensure!(r["text_sha256"] == sha(t.as_bytes()), "current text hash");
            self.exact.insert(sha(t.as_bytes()));
            self.normalized.insert(normalized_sha(t));
            self.families.insert(text(&r, "source_group")?.into());
        }
        ensure!(seen == ids, "incomplete current population");
        Ok(())
    }
}
fn historical_date(r: &Value) -> Option<(&str, &str)> {
    let published = r["source"]["published_at"]
        .as_str()
        .filter(|s| !s.is_empty());
    let (raw, basis) = if let Some(date) = published {
        (date, "source.published_at")
    } else {
        (
            r["evidence_notes"]
                .as_str()?
                .split_once("Historical revision timestamp: ")?
                .1,
            "original_evidence_notes_historical_revision_timestamp",
        )
    };
    let date = raw.get(..10)?;
    let b = date.as_bytes();
    if b[4] != b'-'
        || b[7] != b'-'
        || !b
            .iter()
            .enumerate()
            .all(|(i, c)| i == 4 || i == 7 || c.is_ascii_digit())
        || !("01"..="12").contains(&&date[5..7])
        || !("01"..="31").contains(&&date[8..10])
    {
        return None;
    }
    Some((date, basis))
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
    if !COLLECTIONS.contains(&r["source"]["collection"].as_str().unwrap_or("")) {
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
    if ["license", "evidence_url", "evidence_sha256", "attribution"]
        .iter()
        .any(|k| r["rights"][k].as_str().is_none_or(str::is_empty))
    {
        reasons.push("missing_rights_evidence".into());
    }
    if ["url", "version", "raw_sha256", "extraction"]
        .iter()
        .any(|k| r["source"][k].as_str().is_none_or(str::is_empty))
    {
        reasons.push("missing_source_provenance".into());
    }
    if historical_date(r).is_none_or(|(date, _)| date >= "2022-11-01") {
        reasons.push("missing_or_nonhistorical_publication".into());
    }
    reasons
}
struct Candidate {
    record: Value,
    line_sha: String,
    rank: String,
    word_count: usize,
}
fn rank(group: &str) -> String {
    sha(format!("{DOMAIN}{group}").as_bytes())
}
fn select(
    mut candidates: Vec<Candidate>,
    protected: &Protected,
    quota: usize,
) -> (Vec<Candidate>, Vec<Value>, Value) {
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
    let mut families = protected.families.clone();
    let mut exact = protected.exact.clone();
    let mut normalized = protected.normalized.clone();
    let mut eligible = BTreeMap::<String, usize>::new();
    let mut chosen = BTreeMap::<String, usize>::new();
    let mut selected = Vec::new();
    let mut audit = Vec::new();
    for c in candidates {
        let r = &c.record;
        let group = r["source_group"].as_str().unwrap();
        let t = r["text"].as_str().unwrap();
        let collection = r["source"]["collection"].as_str().unwrap();
        let norm = normalized_sha(t);
        let mut reasons = Vec::new();
        if families.contains(group) {
            reasons.push("duplicate_or_protected_family");
        }
        if exact.contains(r["text_sha256"].as_str().unwrap()) {
            reasons.push("duplicate_exact_text");
        }
        if normalized.contains(&norm) {
            reasons.push("duplicate_normalized_text");
        }
        if reasons.is_empty() {
            families.insert(group.into());
            exact.insert(r["text_sha256"].as_str().unwrap().into());
            normalized.insert(norm.clone());
            *eligible.entry(collection.into()).or_default() += 1;
            if *chosen.get(collection).unwrap_or(&0) >= quota {
                reasons.push("collection_quota");
            } else {
                *chosen.entry(collection.into()).or_default() += 1;
            }
        }
        audit.push(json!({"original_id":r["id"],"source_group":group,"collection":collection,"words":c.word_count,"text_sha256":r["text_sha256"],"normalized_text_sha256":norm,"rank_sha256":c.rank,"selected":reasons.is_empty(),"reasons":reasons}));
        if reasons.is_empty() {
            selected.push(c);
        }
    }
    let counts=json!(COLLECTIONS.iter().map(|k|json!({"collection":k,"eligible":eligible.get(*k).copied().unwrap_or(0),"selected":chosen.get(*k).copied().unwrap_or(0),"requested":quota,"shortfall":quota-chosen.get(*k).copied().unwrap_or(0)})).collect::<Vec<_>>());
    (selected, audit, counts)
}
fn supervised(c: &Candidate, train_sha: &str, manifest_sha: &str) -> Result<Value> {
    let r = &c.record;
    let t = text(r, "text")?;
    let mut history = r.clone();
    history
        .as_object_mut()
        .context("record object")?
        .remove("text");
    Ok(
        json!({"schema":"slop_ninja_a_supervised_record_v1","id":format!("train-scale-human-{}",sha(text(r,"id")?.as_bytes())),
        "text":t,"text_sha256":r["text_sha256"],"source_group":r["source_group"],"frozen_split":"train","purpose":"query",
        "origin":"human_only","evidence":"historical_proxy","documented_human_only":false,
        "human_identity":null,"human_role_present":true,"ai_identity":null,"ai_role_present":false,
        "share_target":0.0,"share_evidence":{"basis":"historical_human_proxy_endpoint","not_documented_contemporary_workflow":true},
        "exposed_to_grammar":null,"grammar_exposure":"unknown",
        "metadata":{"source_collection":r["source"]["collection"],"original_record_id":r["id"],"word_count":c.word_count,"selection_rank_sha256":c.rank,"v6_upstream_exposure":"original_train",
            "historical_date":historical_date(r).map(|(date,_)|date),"historical_date_basis":historical_date(r).map(|(_,basis)|basis)},
        "source":{"record":history,"original_train_sha256":train_sha,"source_manifest_sha256":manifest_sha,"original_jsonl_line_sha256":c.line_sha,
            "byte_span":[0,t.len()],"extraction":"unchanged_whole_original_v6_train_human_record_v1","original_text_sha256":r["text_sha256"]},
        "rights":r["rights"],"parent_id":null}),
    )
}
fn run(a: &[String]) -> Result<()> {
    ensure!(
        a.len() == 7,
        "select_train_augmentation V6_SNAPSHOT V6_CREDITS CURRENT_DATASET CURRENT_REFERENCES SELECTION_TSV PROTOCOL NEW_OUT"
    );
    let out = Path::new(&a[6]);
    ensure!(!out.exists(), "new output directory required");
    let mut pins = BTreeMap::new();
    let protocol = read(Path::new(&a[5]), &mut pins)?;
    ensure!(
        sha(&protocol) == PROTOCOL_SHA,
        "different frozen selection protocol"
    );
    let mut protected = Protected::default();
    ensure!(
        protected.roster(&read(Path::new(&a[4]), &mut pins)?)? == 72,
        "72-work reserved roster required"
    );
    let snapshot = Path::new(&a[0]);
    let mb = read(&snapshot.join("manifest.json"), &mut pins)?;
    let m: Value = serde_json::from_slice(&mb)?;
    ensure!(
        m["schema"] == "slop_ninja_origin_shards_v1",
        "original snapshot schema"
    );
    let cb = read(Path::new(&a[1]), &mut pins)?;
    let credits: Value = serde_json::from_slice(&cb)?;
    ensure!(
        credits["schema"] == "slop_ninja_corpus_attribution_v2"
            && credits["provenance"]["test_partition_opened_by_extractor"] == false,
        "metadata-only credits required"
    );
    ensure!(
        credits["partitions"]["train"] == m["partitions"]["train"],
        "credits/snapshot binding"
    );
    let mut metadata = BTreeMap::new();
    let mut prior_protected = BTreeSet::new();
    for r in credits["records"].as_array().context("credits records")? {
        ensure!(
            r.get("text").is_none(),
            "credits unexpectedly contains prose"
        );
        if r["split"] == "train" {
            ensure!(
                metadata.insert(text(r, "id")?.to_owned(), r).is_none(),
                "duplicate metadata ID"
            );
        } else {
            prior_protected.insert(text(r, "source_group")?.to_owned());
        }
    }
    protected.families.extend(prior_protected.iter().cloned());
    protected.current(Path::new(&a[2]), 929, false, &mut pins)?;
    protected.current(Path::new(&a[3]), 8, true, &mut pins)?;
    let bytes = read(&snapshot.join("train.jsonl"), &mut pins)?;
    let train_sha = sha(&bytes);
    ensure!(
        m["partitions"]["train"]["sha256"] == train_sha,
        "original Train digest changed"
    );
    let mut candidates = Vec::new();
    let mut early = Vec::new();
    let mut seen = BTreeSet::new();
    let mut root_counts = BTreeMap::<String, usize>::new();
    for line in std::str::from_utf8(&bytes)?.lines() {
        let r: Value = serde_json::from_str(line)?;
        ensure!(
            r["split"] == "train",
            "non-Train row in original Train file"
        );
        let id = text(&r, "id")?;
        ensure!(seen.insert(id.to_owned()), "duplicate original ID");
        let meta = metadata.get(id).context("unbound original ID")?;
        ensure!(
            r["source_group"] == meta["source_group"]
                && r["text_sha256"] == meta["text_sha256"]
                && r["origin"] == meta["origin"]
                && r["evidence"] == meta["evidence"],
            "original metadata mismatch"
        );
        if r["origin"] != "human_only" {
            continue;
        }
        *root_counts
            .entry(text(&r["source"], "collection")?.into())
            .or_default() += 1;
        let mut reasons = root_reasons(&r);
        reasons.extend(protected.metadata_reasons(&r));
        // Protected metadata is checked before inspecting candidate prose.
        if !reasons.is_empty() {
            early.push(json!({"original_id":id,"source_group":r["source_group"],"collection":r["source"]["collection"],"selected":false,"reasons":reasons}));
            continue;
        }
        let t = text(&r, "text")?;
        ensure!(
            r["text_sha256"] == sha(t.as_bytes()),
            "original human text hash"
        );
        let n = words(t);
        if !(150..=450).contains(&n) {
            early.push(json!({"original_id":id,"source_group":r["source_group"],"collection":r["source"]["collection"],"selected":false,"words":n,"reasons":["word_count_outside_150_450"]}));
            continue;
        }
        candidates.push(Candidate {
            rank: rank(text(&r, "source_group")?),
            record: r,
            line_sha: sha(line.as_bytes()),
            word_count: n,
        });
    }
    ensure!(
        seen.len()
            == m["partitions"]["train"]["rows"]
                .as_u64()
                .context("Train count")? as usize
            && seen.len() == metadata.len(),
        "incomplete original Train roster"
    );
    let (selected, mut audit, counts) = select(candidates, &protected, QUOTA);
    audit.extend(early);
    audit.sort_by(|a, b| a["original_id"].as_str().cmp(&b["original_id"].as_str()));
    let humans = selected
        .iter()
        .map(|c| supervised(c, &train_sha, &sha(&mb)))
        .collect::<Result<Vec<_>>>()?;
    let hb = jsonl(&humans)?;
    let ab = jsonl(&audit)?;
    let selection = json!({"schema":"slop_ninja_train_augmentation_selection_v1","protocol_sha256":PROTOCOL_SHA,"rank_domain":"slop_ninja_train_augmentation_roots_v1\\0","collections":counts,
        "selected":selected.iter().map(|c|json!({"original_id":c.record["id"],"source_group":c.record["source_group"],"collection":c.record["source"]["collection"],"rank_sha256":c.rank,"text_sha256":c.record["text_sha256"],"words":c.word_count})).collect::<Vec<_>>()});
    let sb = serde_json::to_vec_pretty(&selection)?;
    let manifest = json!({"schema":"slop_ninja_train_augmentation_roots_v1","requested":96,"records":humans.len(),"records_file":"humans.jsonl","records_sha256":sha(&hb),"selection_sha256":sha(&sb),"exclusion_audit_sha256":sha(&ab),
        "complete":humans.len()==96,"shortfall":96-humans.len(),"collections":counts,"original_human_roots_by_collection":root_counts,"original_train_rows":seen.len(),"input_files":pins,
        "protocol_sha256":PROTOCOL_SHA,"implementation_sha256":sha(include_bytes!("select_train_augmentation.rs")),"prior_protected_families_from_metadata":prior_protected.len(),
        "protected_current_non_test_records":937,"protected_current_roster_works":72,"test_text_opened":false,"model_calls":0,"downloads":0,"normalization_fitted":false,
        "human_identity_policy":"unknown individual writer null; coauthor/collective attribution retained in original source",
        "historical_evidence":"historical_proxy_zero_share_endpoint_not_documented_human_only",
        "exposure":"V6 original Train and unknown CFG exposure; Mistral existed in frozen upstream; no claim of clean holdout",
        "limits":["title/known-author metadata blacklist does not prove complete alias or subject-book author coverage","word count is not V6 tokenizer qualification","shortfalls remain within each collection without substitutions"],
        "non_test_ids":humans.iter().map(|r|r["id"].clone()).collect::<Vec<_>>(),"test_ids":[]});
    fs::create_dir(out)?;
    put(&out.join("humans.jsonl"), &hb)?;
    put(&out.join("selection.json"), &sb)?;
    put(&out.join("exclusion-audit.jsonl"), &ab)?;
    put(
        &out.join("manifest.json"),
        &serde_json::to_vec_pretty(&manifest)?,
    )?;
    fs::File::open(out)?.sync_all()?;
    println!(
        "{}",
        json!({"selected":humans.len(),"requested":96,"collections":counts,"records_sha256":sha(&hb)})
    );
    Ok(())
}
fn main() -> Result<()> {
    run(&std::env::args().skip(1).collect::<Vec<_>>())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn candidate(id: &str, collection: &str, t: &str) -> Candidate {
        let r = json!({"id":id,"source_group":format!("family:{id}"),"split":"train","origin":"human_only","evidence":"historical_proxy","parent_id":null,"generation":null,
            "text":t,"text_sha256":sha(t.as_bytes()),"source":{"collection":collection,"published_at":"2018-01-01","url":"https://example.invalid/work","version":"v1","raw_sha256":"a".repeat(64),"extraction":"unchanged","author_ids":[]},
            "rights":{"license":"CC-BY-4.0","attribution":"Synthetic fixture","evidence_url":"https://example.invalid/license","evidence_sha256":"b".repeat(64),"commercial_training":true,"model_release":true,"external_evaluation":true,"redistribute_text":true}});
        Candidate {
            rank: rank(r["source_group"].as_str().unwrap()),
            record: r,
            line_sha: "c".repeat(64),
            word_count: words(t),
        }
    }
    #[test]
    fn inclusive_word_bounds_and_unicode_whitespace() {
        for n in [149, 150, 450, 451] {
            let t = vec!["word"; n].join("\u{2003}");
            assert_eq!(words(&t), n);
            assert_eq!((150..=450).contains(&words(&t)), n == 150 || n == 450);
        }
    }
    #[test]
    fn protected_titles_subtitles_and_withheld_names() {
        let mut p = Protected::default();
        p.titles
            .extend(title_keys("The Secret Agent: A Simple Tale"));
        p.authors.insert(author_key("Collins, Wilkie"));
        let mut c = candidate("a", COLLECTIONS[0], "synthetic");
        c.record["rights"]["share_alike"] = json!({"source_title":"Secret Agent (novel)"});
        assert!(
            p.metadata_reasons(&c.record)
                .contains(&"protected_title".into())
        );
        c.record["rights"]["share_alike"]["source_title"] = json!("Secret Agent of Terra");
        assert!(p.metadata_reasons(&c.record).is_empty());
        c.record["source"]["author_ids"] = json!(["Wilkie Collins"]);
        assert!(
            p.metadata_reasons(&c.record)
                .contains(&"protected_withheld_author".into())
        );
    }
    #[test]
    fn roots_require_train_history_rights_and_no_ai_ancestry() {
        let c = candidate("a", COLLECTIONS[0], "synthetic");
        assert!(root_reasons(&c.record).is_empty());
        for (key, value) in [
            ("split", json!("test")),
            ("evidence", json!("documented_human")),
            ("generation", json!({"operation":"draft"})),
            ("parent_id", json!("ancestor")),
        ] {
            let mut r = c.record.clone();
            r[key] = value;
            assert!(!root_reasons(&r).is_empty());
        }
        let mut r = c.record.clone();
        r["rights"]["commercial_training"] = json!(false);
        assert!(!root_reasons(&r).is_empty());
        let mut r = c.record.clone();
        r["source"]["published_at"] = json!("2022-11-01");
        assert!(!root_reasons(&r).is_empty());
    }
    #[test]
    fn wikinews_pinned_historical_revision_date_is_valid_proxy_evidence() {
        let mut r = candidate("news", COLLECTIONS[3], "synthetic").record;
        r["source"]["published_at"] = Value::Null;
        r["evidence_notes"] = json!(
            "Historical proxy. Historical revision timestamp: 2015-09-27T23:42:03Z. Retained source."
        );
        assert!(root_reasons(&r).is_empty());
        assert_eq!(
            historical_date(&r),
            Some((
                "2015-09-27",
                "original_evidence_notes_historical_revision_timestamp"
            ))
        );
        for notes in [
            "Published long ago",
            "Historical revision timestamp: 2022-11-01T00:00:00Z",
            "Historical revision timestamp: 2015-99-99",
        ] {
            r["evidence_notes"] = json!(notes);
            assert!(root_reasons(&r).contains(&"missing_or_nonhistorical_publication".into()));
        }
    }
    #[test]
    fn deterministic_ranking_dedup_and_collection_shortfall() {
        let make = || {
            vec![
                candidate("a", COLLECTIONS[0], "one two"),
                candidate("b", COLLECTIONS[0], "one\n two"),
                candidate("c", COLLECTIONS[0], "different"),
                candidate("d", COLLECTIONS[1], "protected text"),
            ]
        };
        let mut p = Protected::default();
        p.normalized.insert(normalized_sha("protected\ttext"));
        let (a, audit, counts) = select(make(), &p, 2);
        let mut reverse = make();
        reverse.reverse();
        let (b, _, _) = select(reverse, &p, 2);
        assert_eq!(
            a.iter().map(|c| &c.rank).collect::<Vec<_>>(),
            b.iter().map(|c| &c.rank).collect::<Vec<_>>()
        );
        assert_eq!(a.len(), 2);
        assert_eq!(counts[1]["shortfall"], 2);
        assert_eq!(counts[2]["selected"], 0);
        assert_eq!(
            audit
                .iter()
                .filter(|r| r["reasons"]
                    .as_array()
                    .unwrap()
                    .contains(&json!("duplicate_normalized_text")))
                .count(),
            2
        );
    }
    #[test]
    fn exact_bytes_history_and_unknown_identity_survive() {
        let c = candidate("a", COLLECTIONS[2], "  Synthetic\n\nprose.\t ");
        let r = supervised(&c, "train", "manifest").unwrap();
        assert_eq!(r["text"], c.record["text"]);
        assert_eq!(r["rights"], c.record["rights"]);
        assert_eq!(r["source"]["record"]["source"], c.record["source"]);
        assert!(r["human_identity"].is_null());
        assert_eq!(r["share_target"], 0.0);
        assert_eq!(r["documented_human_only"], false);
        assert_eq!(r["metadata"]["source_collection"], COLLECTIONS[2]);
        assert_eq!(
            r["source"]["byte_span"][1],
            c.record["text"].as_str().unwrap().len()
        );
    }
    #[test]
    fn current_population_rejects_test_and_changed_hash() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let c = candidate("a", COLLECTIONS[0], "synthetic");
        let mut r = supervised(&c, "train", "manifest").unwrap();
        r["frozen_split"] = json!("test");
        let b = jsonl(&[r.clone()]).unwrap();
        fs::write(dir.join("records.jsonl"), &b).unwrap();
        fs::write(dir.join("manifest.json"),serde_json::to_vec(&json!({"test_text_opened":false,"non_test_ids":[r["id"]],"records_sha256":sha(&b)})).unwrap()).unwrap();
        assert!(
            Protected::default()
                .current(dir, 1, false, &mut BTreeMap::new())
                .unwrap_err()
                .to_string()
                .contains("Test/unknown")
        );
        fs::write(dir.join("records.jsonl"), b"changed").unwrap();
        assert!(
            Protected::default()
                .current(dir, 1, false, &mut BTreeMap::new())
                .unwrap_err()
                .to_string()
                .contains("records changed")
        );
    }
}
