//! Verify the frozen book admission chain and exact original PG byte spans.
use super::*;

pub(super) fn root_manifest(m: &Value) -> Result<()> {
    ensure!(
        m["schema"] == "slop_ninja_train_book_roots_v1"
            && m["complete"] == true
            && m["test_text_opened"] == false
            && ids(m, "test_ids")?.is_empty(),
        "book admission scope"
    );
    ensure!(
        m["requested"] == 32
            && m["acquired"] == 29
            && m["records"] == 29
            && m["source_families"] == 29
            && ids(m, "non_test_ids")?.len() == 29
            && array(m, "metadata_exclusions")?.len() == 3,
        "book admission denominator"
    );
    Ok(())
}
pub(super) fn retained_manifest(m: &Value) -> Result<()> {
    ensure!(
        m["schema"] == "slop_ninja_retained_train_roots_v1"
            && m["complete"] == true
            && m["train_only"] == true
            && m["test_text_opened"] == false
            && ids(m, "test_ids")?.is_empty()
            && ids(m, "non_test_ids")?.len() == 196
            && m["records"] == 196
            && m["requested"] == 196
            && m["excluded"] == 0,
        "prospective admitted 196 Train roots required"
    );
    Ok(())
}
fn pg(v: &Value) -> Result<u64> {
    v["pg_id"]
        .as_u64()
        .or_else(|| v["pg_id"].as_str().and_then(|s| s.parse().ok()))
        .context("PG id")
}
fn by_pg(rows: &[Value]) -> Result<BTreeMap<u64, &Value>> {
    let mut m = BTreeMap::new();
    for r in rows {
        ensure!(m.insert(pg(r)?, r).is_none(), "duplicate PG ID");
    }
    Ok(m)
}
pub(super) fn source_scope(
    source: &Value,
    selected: &Value,
    known: &BTreeSet<String>,
) -> Result<()> {
    ensure!(
        source["split"] == "train"
            && source["preassigned_partition"] == "train"
            && source["role"] == "query"
            && source["purpose"] == "query"
            && source["enrollment"] == "known",
        "book acquisition must be Train query before any source read"
    );
    ensure!(
        known.contains(str_at(source, "author_key")?)
            && source["author_key"] == selected["human_identity"]
            && source["source_group"] == selected["source_group"]
            && source["underlying_work_key"] == selected["source_group"],
        "book writer/family escaped admission"
    );
    let c = &selected["candidate"];
    ensure!(
        pg(c)? == pg(source)?
            && c["known_author_key"] == source["author_key"]
            && c["languages"] == json!(["en"])
            && c["copyright_status"] == "Public domain in the USA."
            && array(c, "creators")?.len() == 1
            && array(c, "checks_failed")?.is_empty()
            && c["creators"][0]["name"] == source["catalog_author"],
        "catalog author/language/rights"
    );
    let title = str_at(c, "item_title")?
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    ensure!(
        source["title"] == title
            && selected["review"]["decision"] == "admit_metadata"
            && source["catalog_evidence"]["sha256"] == c["metadata_sha256"]
            && source["rights"]["evidence_sha256"] == c["metadata_sha256"]
            && source["rights"]["commercial_training"] == true
            && source["rights"]["external_evaluation"] == true
            && source["rights"]["catalog_statement"] == "Public domain in the USA."
            && source["rights"]["jurisdiction"] == "US",
        "book metadata admission/rights mismatch"
    );
    ensure!(
        source["raw_download"]["sha256"] == source["text_sha256"]
            && source["raw_download"]["bytes"] == source["text_bytes"]
            && source["raw_download"]["status"] == 200,
        "original HTTP byte binding"
    );
    Ok(())
}
pub(super) fn roots(p: &Value, inputs: &mut Vec<Value>) -> Result<(Vec<Value>, Value)> {
    let m = pinned_json(&p["roots_manifest"], inputs)?;
    root_manifest(&m)?;
    ensure!(
        m["records_sha256"] == p["roots_records_sha256"]
            && m["metadata_exclusions"] == p["metadata_exclusions"]
            && m["implementation_sha256"] == p["admission_implementation_sha256"],
        "root manifest bindings"
    );
    let root_dir = Path::new(str_at(&p["roots_manifest"], "path")?)
        .parent()
        .context("root dir")?;
    let bp = pinned_json(&m["plan"], inputs)?;
    ensure!(
        bp["schema"] == "slop_ninja_train_books_plan_v1"
            && bp["requested"] == 32
            && bp["test_text_opened"] == false
            && bp["metadata_exclusions"] == m["metadata_exclusions"]
            && bp["implementation_sha256"] == p["admission_implementation_sha256"],
        "source admission plan"
    );
    let plan_dir = Path::new(str_at(&m["plan"], "path")?)
        .parent()
        .context("book plan dir")?;
    pinned(
        &json!({"path":plan_dir.join("PROTOCOL.txt"),"sha256":bp["protocol_sha256"]}),
        inputs,
    )?;
    let selection = pinned(
        &json!({"path":plan_dir.join("selection.tsv"),"sha256":bp["selection_sha256"]}),
        inputs,
    )?;
    for pin in bp["inputs"]
        .as_object()
        .context("book plan inputs")?
        .values()
    {
        pinned(pin, inputs)?;
    }
    let population = pinned_json(&bp["inputs"]["population_plan"], inputs)?;
    ensure!(
        bp["population_plan"] == population
            && p["numeric_contract"] == population["files"]["numeric_contract"],
        "population/known roster pin mismatch"
    );
    for spec in array(p, "protected_populations")? {
        let original = array(&population, "populations")?
            .iter()
            .find(|x| x["kind"] == spec["kind"])
            .context("protected kind absent")?;
        ensure!(
            spec["manifest"] == population["files"][str_at(original, "manifest_key")?]
                && spec["records"] == original["records"]
                && spec["count"] == original["count"]
                && spec["allowed_splits"] == original["allowed_splits"]
                && spec["references_only"] == original["references_only"],
            "cumulative population substituted"
        );
    }
    let contract = pinned_json(&p["numeric_contract"], inputs)?;
    let known = ids(&contract, "human_profiles")?;
    ensure!(known.len() == 6, "established six-writer roster");
    let selected = by_pg(array(&bp, "selected")?)?;
    ensure!(selected.len() == 29, "admitted source roster");
    let alias_review = pinned_json(&bp["inputs"]["alias_review"], inputs)?;
    ensure!(
        alias_review["schema"] == "slop_ninja_train_book_alias_review_v1",
        "alias review schema"
    );
    for key in ["candidates", "reservations", "prior_reservations"] {
        ensure!(
            alias_review["inputs"][format!("{key}_sha256")] == bp["inputs"][key]["sha256"],
            "alias review binding"
        );
    }
    let reviewed = by_pg(array(&alias_review, "items")?)?;
    ensure!(reviewed.len() == 32, "alias review requested population");
    let acquisition = pinned_json(&m["acquisition_manifest"], inputs)?;
    let acq_dir = Path::new(str_at(&m["acquisition_manifest"], "path")?)
        .parent()
        .context("acquisition dir")?;
    ensure!(
        acquisition["schema"] == "slop_ninja.author_sources.v1"
            && acquisition["complete"] == true
            && acquisition["selection_sha256"] == bp["selection_sha256"]
            && acquisition["excluded_test_sources"] == 0
            && acquisition["reserved_sources"]
                .as_array()
                .is_none_or(|a| a.is_empty()),
        "acquisition scope/binding"
    );
    let sources = by_pg(array(&acquisition, "sources")?)?;
    ensure!(
        sources.keys().eq(selected.keys()),
        "acquisition exact roster"
    );
    let mut lines = std::str::from_utf8(&selection)?.lines();
    let header = lines
        .next()
        .context("selection header")?
        .split('\t')
        .collect::<Vec<_>>();
    let mut selection_rows = BTreeMap::new();
    for line in lines {
        let fields = line.split('\t').collect::<Vec<_>>();
        ensure!(header.len() == fields.len(), "selection row width");
        let row = Value::Object(
            header
                .iter()
                .zip(fields)
                .map(|(k, v)| (k.to_string(), json!(v)))
                .collect(),
        );
        ensure!(
            selection_rows.insert(pg(&row)?, row).is_none(),
            "duplicate selection row"
        );
    }
    ensure!(
        selection_rows.keys().eq(selected.keys()),
        "selection TSV roster"
    );
    let mut families = BTreeSet::new();
    let mut hashes = BTreeSet::new();
    // Complete metadata preflight precedes reading even the first book.
    for (&pg, source) in &sources {
        source_scope(source, selected[&pg], &known)?;
        ensure!(
            selected[&pg]["review"] == **reviewed.get(&pg).context("missing alias decision")?
                && source["selection_row"] == selection_rows[&pg],
            "acquisition selection/alias binding"
        );
        ensure!(
            families.insert(str_at(source, "source_group")?)
                && hashes.insert(digest_at(source, "text_sha256")?),
            "duplicate underlying work/source bytes"
        );
    }
    let ex = pinned_json(&m["excerpt_manifest"], inputs)?;
    let ex_dir = Path::new(str_at(&m["excerpt_manifest"], "path")?)
        .parent()
        .context("excerpt dir")?;
    ensure!(
        ex["schema"] == "slop_ninja_author_queries_v2"
            && ex["mode"] == "fresh-balanced"
            && ex["source_manifest_sha256"] == m["acquisition_manifest"]["sha256"]
            && ex["test_text_opened"] == false
            && ex["test_mode"] == false
            && ids(&ex, "test_ids")?.is_empty()
            && ids(&ex, "reference_ids")?.is_empty()
            && array(&ex, "failures")?.is_empty()
            && ex["records"] == 58,
        "excerpt manifest scope/binding"
    );
    let wanted = selected
        .keys()
        .flat_map(|pg| [format!("author-pg{pg}-p20"), format!("author-pg{pg}-p60")])
        .collect::<BTreeSet<_>>();
    ensure!(
        ids(&ex, "non_test_ids")? == wanted
            && ids(&ex, "emitted_human_ids")? == wanted
            && ids(&ex, "all_ids")? == wanted,
        "excerpt manifest exact IDs"
    );
    let er = records(&pinned(
        &json!({"path":ex_dir.join("humans.jsonl"),"sha256":ex["records_sha256"]}),
        inputs,
    )?)?;
    let excerpt_map = unique_rows(&er)?;
    ensure!(
        excerpt_map.keys().cloned().collect::<BTreeSet<_>>() == wanted,
        "excerpt records roster"
    );
    let rr = records(&pinned(
        &json!({"path":root_dir.join("humans.jsonl"),"sha256":m["records_sha256"]}),
        inputs,
    )?)?;
    let root_map = unique_rows(&rr)?;
    ensure!(
        root_map.keys().cloned().collect::<BTreeSet<_>>() == ids(&m, "non_test_ids")?,
        "root record exact roster"
    );
    let audits = records(&pinned(
        &json!({"path":root_dir.join("exclusion-audit.jsonl"),"sha256":m["audit_sha256"]}),
        inputs,
    )?)?;
    let audit = by_pg(&audits)?;
    ensure!(audit.keys().eq(selected.keys()), "admission audit roster");
    for (&pg, source) in &sources {
        let rdf = &source["catalog_evidence"];
        pinned(
            &json!({"path":acq_dir.join(str_at(rdf,"path")?),"sha256":rdf["sha256"]}),
            inputs,
        )?;
        for receipt in array(&source["raw_download"], "receipts")? {
            pinned(
                &json!({"path":acq_dir.join(str_at(receipt,"path")?),"sha256":receipt["sha256"]}),
                inputs,
            )?;
        }
        let raw = pinned(
            &json!({"path":str_at(source,"text_path")?,"sha256":source["text_sha256"]}),
            inputs,
        )?;
        ensure!(
            raw.len() as u64 == counter(source, "text_bytes")?,
            "book byte count"
        );
        let raw = std::str::from_utf8(&raw)?;
        for fraction in [20, 60] {
            let id = format!("author-pg{pg}-p{fraction}");
            let r = excerpt_map[&id];
            let expected = json!({"frozen_split":"train","purpose":"query","source_group":source["source_group"]});
            balanced::validate_human(
                r,
                source,
                &expected,
                digest_at(&m["acquisition_manifest"], "sha256")?,
            )?;
            let (start, end) = recipe::excerpt(raw, fraction)?;
            ensure!(
                r["source"]["byte_span"] == json!([start, end])
                    && r["text"] == raw[start..end]
                    && r["source"]["extraction"]
                        == "body_percent20_60_next_sentence_220words_end_sentence_max600_v1",
                "excerpt differs from deterministic original byte span"
            );
            ensure!(
                r["exposed_to_grammar"] == true
                    && r["grammar_exposure"]
                        == "conservative_unknown_prior_exposure;fresh_acquisition_is_not_an_exposure_audit",
                "book exposure caveat changed"
            );
        }
        let excerpt = excerpt_map[&format!("author-pg{pg}-p20")];
        let id = format!("train-books-v1-pg{pg}-p20");
        let root = root_map.get(&id).context("fixed p20 root missing")?;
        let mut expected = excerpt.clone();
        expected["id"] = json!(id);
        expected["metadata"] = json!({"source_collection":"project_gutenberg_historical_novel","batch":"large-batch-v1-books",
            "admission_plan_sha256":m["plan"]["sha256"],"source_title":source["title"],
            "underlying_work_aliases":selected[&pg]["aliases"],"word_bound_is_not_tokenizer_qualification":true});
        ensure!(
            **root == expected,
            "admitted root changed exact excerpt/metadata/roles"
        );
        let text = str_at(root, "text")?;
        let n = text.split_whitespace().count();
        let d = audit[&pg];
        ensure!(
            (220..=450).contains(&n)
                && d["selected"] == true
                && array(d, "reasons")?.is_empty()
                && d["words"] == n
                && d["text_sha256"] == root["text_sha256"]
                && d["normalized_text_sha256"] == normalized(text)
                && d["source_group"] == root["source_group"],
            "admitted p20 word/span/audit binding"
        );
    }
    Ok((rr, m))
}
