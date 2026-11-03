//! Train-only book acquisition plan and exact excerpt admission.
//! plan AUDIT_ROOT ALIAS_REVIEW RETAINED_PLAN NEW_OUT
//! admit PLAN_DIR ACQUISITION_DIR EXCERPT_DIR NEW_OUT
//! No HTTP/model calls. Acquisition and extraction use the existing pinned tools.
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, OpenOptions},
    io::Write,
    path::Path,
};

fn sha(b: &[u8]) -> String {
    hex::encode(Sha256::digest(b))
}
fn s<'a>(v: &'a Value, k: &str) -> Result<&'a str> {
    v[k].as_str().with_context(|| format!("missing string {k}"))
}
fn arr<'a>(v: &'a Value, k: &str) -> Result<&'a Vec<Value>> {
    v[k].as_array()
        .with_context(|| format!("missing array {k}"))
}
fn read(p: &Path) -> Result<Value> {
    Ok(serde_json::from_slice(&fs::read(p)?)?)
}
fn rows(b: &[u8]) -> Result<Vec<Value>> {
    std::str::from_utf8(b)?
        .lines()
        .map(|l| Ok(serde_json::from_str(l)?))
        .collect()
}
fn put(p: &Path, b: &[u8]) -> Result<()> {
    let mut f = OpenOptions::new().create_new(true).write(true).open(p)?;
    f.write_all(b)?;
    f.sync_all()?;
    Ok(())
}
fn put_json(p: &Path, v: &Value) -> Result<()> {
    let mut b = serde_json::to_vec_pretty(v)?;
    b.push(b'\n');
    put(p, &b)
}
fn jsonl(v: &[Value]) -> Result<Vec<u8>> {
    let mut b = Vec::new();
    for r in v {
        serde_json::to_writer(&mut b, r)?;
        b.push(b'\n');
    }
    Ok(b)
}
fn normalized(t: &str) -> String {
    sha(t
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .as_bytes())
}
fn title(t: &str) -> String {
    let t = t
        .to_lowercase()
        .chars()
        .filter(|c| !matches!(c, '\'' | '’'))
        .map(|c| if c.is_alphanumeric() { c } else { ' ' })
        .collect::<String>();
    let mut w = t.split_whitespace().collect::<Vec<_>>();
    if w.first().is_some_and(|s| ["the", "a", "an"].contains(s)) {
        w.remove(0);
    }
    w.join(" ")
}
fn titles(t: &str) -> BTreeSet<String> {
    [t, t.split([':', ';', '(', '\n']).next().unwrap_or(t)]
        .into_iter()
        .map(title)
        .filter(|x| !x.is_empty())
        .collect()
}
fn bound(p: &Path) -> Result<Value> {
    Ok(json!({"path":p.canonicalize()?,"sha256":sha(&fs::read(p)?)}))
}
fn verified(v: &Value) -> Result<Vec<u8>> {
    let b = fs::read(s(v, "path")?)?;
    ensure!(
        v["sha256"] == sha(&b),
        "input digest differs: {}",
        s(v, "path")?
    );
    Ok(b)
}
fn id(v: &Value) -> Result<u64> {
    v["pg_id"]
        .as_u64()
        .or_else(|| v["pg_id"].as_str().and_then(|x| x.parse().ok()))
        .context("PG id")
}

const PROTOCOL: &str = "Train-only independent-book admission v1. Freeze this plan before prose reads. Keep all current/prior Train, Development, Calibration, Test and reference families and withheld writers reserved. Candidates are a fixed metadata allow-list, not a quota to fill. The separate alias review may exclude uncertain editions, compilations or unfinished works; every exclusion remains. Acquire only reviewed English single-author US-public-domain items via a_acquire_balanced_sources; preserve original bytes, notices, RDF and HTTP receipts. Extract using prepare_author_queries --fresh-balanced. Admit only the fixed 20-percent body excerpt (next sentence, at least220 words, complete terminal sentence), with a further150..450-whitespace-word bound. The60-percent excerpt is retained but never selected as a replacement. No hand-picked passages, quality/model-score selection, truncation or prose repair. Verify exact byte spans and hashes, cumulative non-Test exact/whitespace-normalized duplicates, prospective retained candidates, and family reservations. Never read Test prose. Historical text is a proxy, not authenticated unaided history; publication/composition dates remain unavailable unless supported by source evidence. Preserve established named writer identities; no identity invention. All descendants inherit Train and the entire underlying work's family. V6 tokenizer qualification remains a later mandatory gate; word count does not establish token fit. Keep grammar/pretraining exposure unknown/conservative, all failures, exact input hashes and requested denominator. No paid calls or generation during admission. No final-Test model selection.";

fn plan(audit: &Path, review_path: &Path, retained: &Path, out: &Path) -> Result<()> {
    let mut inputs = BTreeMap::new();
    for (key, p) in [
        ("candidates", audit.join("pg-priority-candidates.jsonl")),
        ("reservations", audit.join("reservations.json")),
        (
            "prior_reservations",
            audit.join("prior-pilot-reservations.json"),
        ),
        (
            "retained_candidates",
            audit.join("retained-v6-standard-candidates.jsonl"),
        ),
        ("alias_review", review_path.into()),
        ("population_plan", retained.into()),
    ] {
        inputs.insert(key, bound(&p)?);
    }
    let candidates = rows(&verified(&inputs["candidates"])?)?;
    let reservations: Value = serde_json::from_slice(&verified(&inputs["reservations"])?)?;
    let prior: Value = serde_json::from_slice(&verified(&inputs["prior_reservations"])?)?;
    let review: Value = serde_json::from_slice(&verified(&inputs["alias_review"])?)?;
    ensure!(
        review["schema"] == "slop_ninja_train_book_alias_review_v1",
        "alias review schema"
    );
    for k in ["candidates", "reservations", "prior_reservations"] {
        ensure!(
            review["inputs"][format!("{k}_sha256")] == inputs[k]["sha256"],
            "alias review input binding {k}"
        );
    }
    let pop: Value = serde_json::from_slice(&verified(&inputs["population_plan"])?)?;
    ensure!(
        pop["schema"] == "slop_ninja_retained_train_admission_plan_v1",
        "population plan schema"
    );
    let contract: Value = serde_json::from_slice(&verified(&pop["files"]["numeric_contract"])?)?;
    let known: BTreeSet<_> = arr(&contract, "human_profiles")?
        .iter()
        .map(|v| v.as_str().context("human profile"))
        .collect::<Result<_>>()?;
    let mut blocked = BTreeSet::new();
    let mut pgs = BTreeSet::new();
    let mut groups = BTreeSet::new();
    let mut withheld = BTreeSet::new();
    for (v, k) in [
        (&reservations, "whole_author_withheld"),
        (&prior, "withheld_authors"),
    ] {
        for a in arr(v, k)? {
            withheld.insert(a.as_str().context("withheld author")?.to_owned());
        }
    }
    for (v, k) in [(&reservations, "fresh_cohort_works"), (&prior, "works")] {
        for r in arr(v, k)? {
            pgs.insert(id(r)?);
            blocked.extend(titles(s(r, "expected_title")?));
            if let Some(g) = r["underlying_work_key"].as_str() {
                groups.insert(g.to_owned());
            }
        }
    }
    for t in arr(
        &reservations,
        "all_original_v6_wikipedia_source_titles_conservatively_blocked_for_new_cross_corpus_work",
    )? {
        blocked.extend(titles(t.as_str().context("reserved title")?));
    }
    let mut reviewed = BTreeMap::new();
    for r in arr(&review, "items")? {
        ensure!(
            reviewed.insert(id(r)?, r).is_none(),
            "duplicate alias review item"
        );
    }
    ensure!(
        reviewed.len() == candidates.len() && !candidates.is_empty(),
        "incomplete alias review"
    );
    let mut selected = Vec::new();
    let mut excluded = Vec::new();
    let mut seen = BTreeSet::new();
    let mut tsv = String::from(
        "pg_id\tauthor_key\tauthor\tcatalog_author\texpected_title\tunderlying_work_key\tsplit\trole\tenrollment\tauthor_partition\tlanguage\trights\trights_evidence_url\tmetadata_path\tmetadata_sha256\n",
    );
    for r in &candidates {
        let pg = id(r)?;
        ensure!(seen.insert(pg), "duplicate PG candidate");
        let rv = reviewed.get(&pg).context("missing review")?;
        let author = s(r, "known_author_key")?;
        let mut aliases = titles(s(r, "item_title")?);
        aliases.extend(titles(s(r, "canonical_work_title")?));
        for a in arr(rv, "aliases")? {
            aliases.extend(titles(a.as_str().context("alias")?));
        }
        let mut reasons = Vec::new();
        match s(rv, "decision")? {
            "admit_metadata" => {}
            "exclude" => reasons.push(format!("alias_review: {}", s(rv, "reason")?)),
            _ => bail!("invalid review decision"),
        }
        if pgs.contains(&pg) || !aliases.is_disjoint(&blocked) {
            reasons.push("reserved_or_duplicate_underlying_work".into());
        }
        if !known.contains(author) || withheld.contains(author) {
            reasons.push("writer_not_known_or_withheld".into());
        }
        ensure!(
            r["languages"] == json!(["en"])
                && r["copyright_status"] == "Public domain in the USA."
                && arr(r, "checks_failed")?.is_empty()
                && arr(r, "creators")?.len() == 1,
            "candidate item metadata invalid"
        );
        let family = format!(
            "author_work_v2:{author}:{}",
            title(s(r, "canonical_work_title")?).replace(' ', "_")
        );
        if groups.contains(&family) {
            reasons.push("reserved_family".into());
        }
        if !reasons.is_empty() {
            excluded.push(json!({"pg_id":pg,"reasons":reasons,"review":rv}));
            continue;
        }
        let mp = audit.join(s(r, "metadata_file")?);
        ensure!(sha(&fs::read(&mp)?) == r["metadata_sha256"], "RDF changed");
        let title = s(r, "item_title")?
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        let fields = vec![
            pg.to_string(),
            author.into(),
            s(r, "known_author_name")?.into(),
            s(&r["creators"][0], "name")?.into(),
            title,
            family.clone(),
            "train".into(),
            "query".into(),
            "known".into(),
            "all".into(),
            "en".into(),
            "Public domain in the USA.".into(),
            format!("https://www.gutenberg.org/ebooks/{pg}.rdf"),
            s(r, "metadata_file")?.into(),
            s(r, "metadata_sha256")?.into(),
        ];
        ensure!(
            fields.iter().all(|f| !f.contains(['\t', '\n', '\r'])),
            "unsafe TSV cell"
        );
        tsv.push_str(&fields.join("\t"));
        tsv.push('\n');
        selected.push(json!({"pg_id":pg,"source_group":family,"human_identity":author,"candidate":r,"aliases":aliases,"review":rv}));
        blocked.extend(aliases);
        pgs.insert(pg);
        groups.insert(family);
    }
    ensure!(!selected.is_empty(), "no eligible books");
    fs::create_dir(out)?;
    put(&out.join("selection.tsv"), tsv.as_bytes())?;
    put(&out.join("PROTOCOL.txt"), PROTOCOL.as_bytes())?;
    let p = json!({"schema":"slop_ninja_train_books_plan_v1","protocol_sha256":sha(PROTOCOL.as_bytes()),"selection_sha256":sha(tsv.as_bytes()),"metadata_root":audit.canonicalize()?,"inputs":inputs,"population_plan":pop,"requested":candidates.len(),"selected":selected,"metadata_exclusions":excluded,"test_text_opened":false,"implementation_sha256":sha(include_bytes!("prepare_train_books.rs"))});
    put_json(&out.join("plan.json"), &p)?;
    println!(
        "{}",
        json!({"requested":candidates.len(),"acquisition_selected":selected.len(),"excluded":excluded.len()})
    );
    Ok(())
}

fn safe_row(r: &Value) -> Result<()> {
    ensure!(
        matches!(
            s(r, "frozen_split")?,
            "train" | "development" | "calibration"
        ),
        "Test/unknown population prohibited"
    );
    ensure!(
        matches!(s(r, "purpose")?, "query" | "reference"),
        "population purpose"
    );
    ensure!(
        r["text_sha256"] == sha(s(r, "text")?.as_bytes()),
        "population text binding"
    );
    Ok(())
}
fn eligible_excerpt(r: &Value, source: &Value, raw: &str) -> Result<()> {
    ensure!(
        r["frozen_split"] == "train"
            && r["purpose"] == "query"
            && r["origin"] == "human_only"
            && r["documented_human_only"] == false
            && r["parent_id"].is_null(),
        "excerpt role/history"
    );
    ensure!(
        r["source_group"] == source["source_group"]
            && r["human_identity"] == source["author_key"]
            && r["source"]["record"] == *source,
        "excerpt source binding"
    );
    ensure!(
        r["rights"]["commercial_training"] == true && r["rights"]["external_evaluation"] == true,
        "source rights"
    );
    let span = arr(&r["source"], "byte_span")?;
    ensure!(span.len() == 2, "span arity");
    let a = span[0].as_u64().context("span start")? as usize;
    let b = span[1].as_u64().context("span end")? as usize;
    let t = s(r, "text")?;
    ensure!(
        raw.get(a..b) == Some(t) && r["text_sha256"] == sha(t.as_bytes()),
        "exact original span"
    );
    ensure!(
        (150..=450).contains(&t.split_whitespace().count()),
        "word_count_outside150_450"
    );
    Ok(())
}
fn admit(plan_dir: &Path, acq: &Path, ex: &Path, out: &Path) -> Result<()> {
    let p = read(&plan_dir.join("plan.json"))?;
    ensure!(
        p["schema"] == "slop_ninja_train_books_plan_v1"
            && p["protocol_sha256"] == sha(PROTOCOL.as_bytes())
            && fs::read(plan_dir.join("PROTOCOL.txt"))? == PROTOCOL.as_bytes(),
        "plan protocol"
    );
    for v in p["inputs"].as_object().context("inputs")?.values() {
        verified(v)?;
    }
    let ab = fs::read(acq.join("manifest.json"))?;
    let am: Value = serde_json::from_slice(&ab)?;
    ensure!(
        am["complete"] == true
            && am["selection_sha256"] == p["selection_sha256"]
            && sha(&fs::read(plan_dir.join("selection.tsv"))?) == p["selection_sha256"]
            && am["excluded_test_sources"] == 0,
        "acquisition plan binding/scope"
    );
    let selected = arr(&p, "selected")?;
    let sources = arr(&am, "sources")?;
    ensure!(sources.len() == selected.len(), "acquisition roster");
    let mut source_map = BTreeMap::new();
    for source in sources {
        let pg = id(source)?;
        let selected = selected
            .iter()
            .find(|r| id(r).ok() == Some(pg))
            .context("unselected book")?;
        ensure!(
            source["split"] == "train"
                && source["role"] == "query"
                && source["enrollment"] == "known"
                && source["source_group"] == selected["source_group"]
                && source["author_key"] == selected["human_identity"],
            "source scope"
        );
        ensure!(source_map.insert(pg, source).is_none(), "duplicate book");
    }
    let em = read(&ex.join("manifest.json"))?;
    ensure!(
        em["schema"] == "slop_ninja_author_queries_v2"
            && em["source_manifest_sha256"] == sha(&ab)
            && em["test_text_opened"] == false
            && arr(&em, "test_ids")?.is_empty(),
        "excerpt manifest binding"
    );
    let eb = fs::read(ex.join("humans.jsonl"))?;
    ensure!(em["records_sha256"] == sha(&eb), "excerpts changed");
    let er = rows(&eb)?;
    ensure!(em["records"] == er.len(), "excerpt count");
    let mut excerpt_map = BTreeMap::new();
    for r in &er {
        ensure!(
            excerpt_map.insert(s(r, "id")?, r).is_none(),
            "duplicate excerpt id"
        );
    }
    let pop = &p["population_plan"];
    let mut exact = BTreeSet::new();
    let mut norm = BTreeSet::new();
    let mut families = BTreeMap::new();
    let mut ids = BTreeSet::new();
    // Validate manifests and metadata scopes before opening each allowed prose population.
    for population in arr(pop, "populations")? {
        let m: Value =
            serde_json::from_slice(&verified(&pop["files"][s(population, "manifest_key")?])?)?;
        ensure!(
            m["schema"] == "slop_ninja_a_supervised_dataset_v1" && m["test_text_opened"] == false,
            "protected manifest must describe non-Test data"
        );
        let permitted_ids: BTreeSet<_> = arr(&m, "non_test_ids")?
            .iter()
            .map(|v| v.as_str().context("permitted ID"))
            .collect::<Result<_>>()?;
        let test_ids: BTreeSet<_> = arr(&m, "test_ids")?
            .iter()
            .map(|v| v.as_str().context("reserved Test ID"))
            .collect::<Result<_>>()?;
        ensure!(
            permitted_ids.is_disjoint(&test_ids)
                && permitted_ids.len() as u64
                    == population["count"].as_u64().context("population count")?,
            "protected roster"
        );
        ensure!(
            m["records_sha256"] == population["records"]["sha256"],
            "population manifest binding"
        );
        let allowed = arr(population, "allowed_splits")?;
        ensure!(
            allowed
                .iter()
                .all(|v| matches!(v.as_str(), Some("train" | "development" | "calibration"))),
            "protected Test requested"
        );
        let b = verified(&population["records"])?;
        let rr = rows(&b)?;
        ensure!(population["count"] == rr.len(), "population count");
        for r in rr {
            safe_row(&r)?;
            ensure!(
                permitted_ids.contains(s(&r, "id")?)
                    && !test_ids.contains(s(&r, "id")?)
                    && m["split_assignments"][s(&r, "source_group")?] == r["frozen_split"],
                "protected row/manifest scope"
            );
            ensure!(
                allowed.contains(&r["frozen_split"]),
                "unallowed population split"
            );
            ensure!(
                ids.insert(s(&r, "id")?.to_owned()),
                "duplicate cumulative ID"
            );
            if let Some(prior) = families.insert(
                s(&r, "source_group")?.to_owned(),
                s(&r, "frozen_split")?.to_owned(),
            ) {
                ensure!(prior == s(&r, "frozen_split")?, "family split conflict");
            }
            exact.insert(s(&r, "text_sha256")?.to_owned());
            norm.insert(normalized(s(&r, "text")?));
        }
    }
    // The candidate metadata contains exact hashes, not normalized prose hashes.
    // Derive the latter only from the separately pinned original Train shard.
    let candidates = rows(&verified(&p["inputs"]["retained_candidates"])?)?;
    let original = rows(&verified(&pop["files"]["original_train"])?)?;
    let mut originals = BTreeMap::new();
    for r in &original {
        ensure!(r["split"] == "train", "original shard includes non-Train");
        ensure!(
            originals.insert(s(r, "id")?, r).is_none(),
            "duplicate original ID"
        );
    }
    for c in candidates {
        let r = originals
            .get(s(&c, "root_id")?)
            .context("candidate absent from Train")?;
        let t = s(r, "text")?;
        ensure!(
            r["text_sha256"] == sha(t.as_bytes())
                && r["text_sha256"] == c["text_sha256"]
                && r["source_group"] == c["source_group"],
            "candidate original binding"
        );
        exact.insert(sha(t.as_bytes()));
        norm.insert(normalized(t));
    }
    let mut accepted = Vec::new();
    let mut audit = Vec::new();
    for selected in selected {
        let pg = id(selected)?;
        let source = source_map[&pg];
        let excerpt_id = format!("author-pg{pg}-p20");
        let mut reasons = Vec::new();
        let Some(r) = excerpt_map.get(excerpt_id.as_str()) else {
            audit.push(json!({"pg_id":pg,"selected":false,"reasons":["fixed20percent_excerpt_failed"],"extraction_failures":em["failures"]}));
            continue;
        };
        ensure!(
            r["source"]["source_manifest_sha256"] == sha(&ab),
            "excerpt acquisition hash"
        );
        let raw = fs::read(s(source, "text_path")?)?;
        ensure!(source["text_sha256"] == sha(&raw), "book bytes changed");
        if let Err(e) = eligible_excerpt(r, source, std::str::from_utf8(&raw)?) {
            reasons.push(e.to_string());
        }
        let t = s(r, "text")?;
        let hash = sha(t.as_bytes());
        let normalized = normalized(t);
        let group = s(r, "source_group")?;
        if families.contains_key(group) {
            reasons.push("reserved_family".into());
        }
        if exact.contains(&hash) || norm.contains(&normalized) {
            reasons.push("duplicate_cumulative_text".into());
        }
        audit.push(json!({"pg_id":pg,"source_group":group,"selected":reasons.is_empty(),"reasons":reasons,"text_sha256":hash,"normalized_text_sha256":normalized,"words":t.split_whitespace().count()}));
        if !reasons.is_empty() {
            continue;
        }
        let mut row = (*r).clone();
        row["id"] = json!(format!("train-books-v1-pg{pg}-p20"));
        row["metadata"] = json!({"source_collection":"project_gutenberg_historical_novel","batch":"large-batch-v1-books","admission_plan_sha256":sha(&fs::read(plan_dir.join("plan.json"))?),"source_title":source["title"],"underlying_work_aliases":selected["aliases"],"word_bound_is_not_tokenizer_qualification":true});
        families.insert(group.into(), "train".into());
        exact.insert(hash);
        norm.insert(normalized);
        accepted.push(row);
    }
    fs::create_dir(out)?;
    let b = jsonl(&accepted)?;
    put(&out.join("humans.jsonl"), &b)?;
    let a = jsonl(&audit)?;
    put(&out.join("exclusion-audit.jsonl"), &a)?;
    let manifest = json!({"schema":"slop_ninja_train_book_roots_v1","complete":true,"requested":p["requested"],"acquired":selected.len(),"records":accepted.len(),"source_families":accepted.len(),"records_sha256":sha(&b),"audit_sha256":sha(&a),"plan":bound(&plan_dir.join("plan.json"))?,"acquisition_manifest":bound(&acq.join("manifest.json"))?,"excerpt_manifest":bound(&ex.join("manifest.json"))?,"non_test_ids":accepted.iter().map(|r|r["id"].clone()).collect::<Vec<_>>(),"test_ids":[],"test_text_opened":false,"metadata_exclusions":p["metadata_exclusions"],"implementation_sha256":sha(include_bytes!("prepare_train_books.rs")),"tokenizer_qualified":false,"publication_rights":"US public-domain source evidence; preserve PG notices; no automatic publication"});
    put_json(&out.join("manifest.json"), &manifest)?;
    println!(
        "{}",
        json!({"records":accepted.len(),"requested":p["requested"],"acquired":selected.len()})
    );
    Ok(())
}
fn main() -> Result<()> {
    let a = std::env::args().skip(1).collect::<Vec<_>>();
    ensure!(
        a.len() == 5,
        "plan AUDIT ALIAS_REVIEW RETAINED_PLAN OUT | admit PLAN_DIR ACQUISITION_DIR EXCERPT_DIR OUT"
    );
    match a[0].as_str() {
        "plan" => plan(
            Path::new(&a[1]),
            Path::new(&a[2]),
            Path::new(&a[3]),
            Path::new(&a[4]),
        ),
        "admit" => admit(
            Path::new(&a[1]),
            Path::new(&a[2]),
            Path::new(&a[3]),
            Path::new(&a[4]),
        ),
        _ => bail!("unknown command"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn title_aliases_block_articles_apostrophes_subtitles() {
        assert_eq!(title("The Duke’s Children"), title("Duke's Children"));
        assert!(titles("A Laodicean: A Story").contains("laodicean"));
        assert!(titles("Phineas Finn\nThe Irish Member").contains("phineas finn"));
        assert_eq!(title("La Vendée"), "la vendée");
    }
    #[test]
    fn test_and_unknown_partitions_rejected() {
        for split in ["test", "test_reference", "unknown"] {
            assert!(safe_row(&json!({"frozen_split":split,"purpose":"query","text":"x","text_sha256":sha(b"x")})).is_err());
        }
    }
    #[test]
    fn exact_span_and_word_bounds_enforced() {
        let t = "word ".repeat(220);
        let source = json!({"source_group":"synthetic","author_key":"writer"});
        let mut r = json!({"frozen_split":"train","purpose":"query","origin":"human_only","documented_human_only":false,"parent_id":null,"source_group":"synthetic","human_identity":"writer","source":{"record":source,"byte_span":[0,t.len()]},"text":t,"text_sha256":sha(t.as_bytes()),"rights":{"commercial_training":true,"external_evaluation":true}});
        eligible_excerpt(&r, &source, &t).unwrap();
        r["source"]["byte_span"] = json!([1, t.len()]);
        assert!(eligible_excerpt(&r, &source, &t).is_err());
        r["source"]["byte_span"] = json!([0, t.len()]);
        r["human_identity"] = json!("wrong_writer");
        assert!(eligible_excerpt(&r, &source, &t).is_err());
    }
    #[test]
    fn normalization_is_comparison_only() {
        assert_eq!(normalized("one\n two"), normalized("one two"));
        assert_ne!(sha(b"one\n two"), sha(b"one two"));
    }
}
