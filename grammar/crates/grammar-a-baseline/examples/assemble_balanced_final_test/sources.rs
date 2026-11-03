//! Exact reserved source metadata and already-extracted bytes; never reopen books.
use super::*;
pub(super) struct Sources {
    pub works: BTreeMap<u64, Value>,
    pub humans: Vec<Value>,
    pub references: Vec<Value>,
    pub humans_sha: String,
    pub source_sha: String,
    pub query_sha: String,
    pub query_exclusions: Vec<Value>,
    pub reference_exclusions: Vec<Value>,
    pub extraction_failures: Vec<Value>,
}
pub(super) fn works() -> Result<BTreeMap<u64, Value>> {
    ensure!(
        sha(TSV.as_bytes()) == TSV_SHA,
        "compiled reservations changed"
    );
    let mut lines = TSV.lines();
    let header = lines
        .next()
        .context("TSV header")?
        .split('\t')
        .collect::<Vec<_>>();
    let mut out = BTreeMap::new();
    for line in lines {
        let cells = line.split('\t').collect::<Vec<_>>();
        ensure!(cells.len() == header.len(), "TSV width");
        let row = header
            .iter()
            .zip(cells)
            .map(|(k, v)| (k.to_string(), json!(v)))
            .collect::<serde_json::Map<_, _>>();
        let r = Value::Object(row);
        if !matches!(r["split"].as_str(), Some("test" | "test_reference")) {
            continue;
        }
        let pg = str_at(&r, "pg_id")?.parse::<u64>()?;
        let w = json!({"pg_id":pg,"source_group":r["underlying_work_key"],"purpose":r["role"],"selection_row":r});
        ensure!(out.insert(pg, w).is_none(), "duplicate reserved work");
    }
    ensure!(
        out.len() == 18 && out.values().filter(|w| w["purpose"] == "query").count() == 16,
        "reserved 16+2 work roster"
    );
    Ok(out)
}
fn roster(works: &BTreeMap<u64, Value>) -> BTreeMap<String, Value> {
    works.iter().flat_map(|(pg,w)|[20,60].map(move|fraction|(format!("author-pg{pg}-p{fraction}"),
        json!({"pg_id":pg,"fraction":fraction,"source_group":w["source_group"],"purpose":w["purpose"],"frozen_split":"test"})))).collect()
}
pub(super) fn source(r: &Value, w: &Value) -> Result<()> {
    let s = &w["selection_row"];
    ensure!(
        r["pg_id"] == w["pg_id"]
            && r["selection_row"] == *s
            && r["split"] == "test"
            && r["preassigned_partition"] == s["split"]
            && r["purpose"] == w["purpose"]
            && r["role"] == w["purpose"]
            && r["source_group"] == w["source_group"]
            && r["underlying_work_key"] == w["source_group"],
        "source reservation binding"
    );
    for (a, b) in [
        ("author_key", "author_key"),
        ("author", "author"),
        ("catalog_author", "catalog_author"),
        ("title", "expected_title"),
        ("enrollment", "enrollment"),
        ("author_partition", "author_partition"),
    ] {
        ensure!(r[a] == s[b], "reserved source {a}");
    }
    ensure!(
        r["catalog_evidence"]["sha256"] == s["metadata_sha256"]
            && r["rights"]["evidence_sha256"] == s["metadata_sha256"]
            && r["rights"]["commercial_training"] == true
            && r["rights"]["external_evaluation"] == true
            && r["rights"]["jurisdiction"] == "US"
            && r["rights"]["catalog_statement"] == "Public domain in the USA."
            && r["evidence"] == "historical_public_domain_author_proxy",
        "source rights/history binding"
    );
    digest_at(r, "text_sha256")?;
    ensure!(counter(r, "text_bytes")? > 0, "empty acquired work");
    ensure!(
        r["raw_download"]["sha256"] == r["text_sha256"]
            && r["raw_download"]["bytes"] == r["text_bytes"]
            && r["raw_download"]["status"] == 200,
        "source download binding"
    );
    Ok(())
}
fn human(r: &Value, s: &Value, e: &Value, source_sha: &str) -> Result<()> {
    for key in ["purpose", "source_group", "frozen_split"] {
        ensure!(r[key] == e[key], "excerpt escaped reservation: {key}");
    }
    ensure!(
        r["schema"] == "slop_ninja_a_supervised_record_v1"
            && r["origin"] == "human_only"
            && r["human_identity"] == s["author_key"]
            && r["human_role_present"] == true
            && r["ai_role_present"] == false
            && r["ai_identity"].is_null()
            && r["parent_id"].is_null()
            && r.get("generation").is_none()
            && r["share_target"] == 0.0
            && r["evidence"] == "historical_public_domain_author_proxy"
            && r["documented_human_only"] == false,
        "excerpt human proxy roles/history"
    );
    ensure!(
        r["source"]["record"] == *s
            && r["source"]["source_manifest_sha256"] == source_sha
            && r["rights"] == s["rights"]
            && r["source"]["extraction"]
                == "body_percent20_60_next_sentence_220words_end_sentence_max600_v1",
        "excerpt exact source/rights/extraction binding"
    );
    let t = str_at(r, "text")?;
    ensure!(r["text_sha256"] == sha(t.as_bytes()), "excerpt text digest");
    let span = array(&r["source"], "byte_span")?;
    ensure!(span.len() == 2, "source span pair");
    let start = span[0].as_u64().context("span start")?;
    let end = span[1].as_u64().context("span end")?;
    ensure!(
        start < end && end <= counter(s, "text_bytes")? && end - start == t.len() as u64,
        "excerpt span bounds"
    );
    ensure!(
        (220..=600).contains(&t.split_whitespace().count()),
        "frozen excerpt length"
    );
    ensure!(
        r["exposed_to_grammar"].is_boolean() && r["grammar_exposure"].is_string(),
        "explicit exposure evidence"
    );
    let a = &s["grammar_exposure_audit"];
    if a.is_null() {
        ensure!(
            r["exposed_to_grammar"] == true
                && r["grammar_exposure"]
                    == "conservative_unknown_prior_exposure;fresh_acquisition_is_not_an_exposure_audit",
            "unaudited clean exposure assertion"
        );
    } else {
        ensure!(
            a["status"] == "audited"
                && a["text_sha256"] == s["text_sha256"]
                && a["exposed_to_grammar"] == r["exposed_to_grammar"]
                && a["grammar_exposure"] == r["grammar_exposure"],
            "changed source exposure audit"
        );
        digest_at(a, "evidence_sha256")?;
    }
    // Test references remain references in all outputs. Only query records enter
    // Dataset::parse_test/Supervision, whose reference policy is Train-only.
    if r["purpose"] == "query" {
        supervision::Supervision::parse(r)?;
    }
    Ok(())
}
pub(super) fn read(plan: &Value, selection_sha: &str, inputs: &mut Vec<Value>) -> Result<Sources> {
    let source_bytes = pinned(&plan["source_manifest"], inputs)?;
    let m: Value = serde_json::from_slice(&source_bytes)?;
    let source_sha = sha(&source_bytes);
    ensure!(
        m["schema"] == "slop_ninja.author_sources.v1"
            && m["acquisition_schema"] == "slop_ninja_balanced_final_test_acquisition_v1"
            && m["complete"] == true
            && m["test_text_opened"] == true
            && m["selection_receipt_sha256"] == selection_sha
            && m["selection_sha256"] == TSV_SHA
            && m["source_count"] == 18
            && m["query_count"] == 16
            && m["reference_count"] == 2,
        "receipt-gated exact Test acquisition required"
    );
    ensure!(
        array(&m, "reserved_sources")?.is_empty(),
        "metadata-only sources cannot be assembled"
    );
    let works = works()?;
    let expected = roster(&works);
    let mut sources = BTreeMap::new();
    for r in array(&m, "sources")? {
        let pg = counter(r, "pg_id")?;
        source(r, works.get(&pg).context("unreserved PG source")?)?;
        ensure!(sources.insert(pg, r).is_none(), "duplicate acquired source");
    }
    ensure!(sources.len() == 18, "missing reserved acquisition");
    let qb = pinned(&plan["query_manifest"], inputs)?;
    let q: Value = serde_json::from_slice(&qb)?;
    ensure!(
        q["schema"] == "slop_ninja_author_queries_v2"
            && q["mode"] == "fresh-balanced"
            && q["test_mode"] == true
            && q["test_text_opened"] == true
            && q["source_manifest_sha256"] == source_sha
            && q["non_test_ids"] == json!([]),
        "Test extractor binding"
    );
    let planned = unique_rows(array(&q, "planned_records")?)?;
    ensure!(
        planned.keys().eq(expected.keys()),
        "exact Test excerpt roster required"
    );
    for (id, e) in &expected {
        for k in ["frozen_split", "purpose", "source_group", "fraction"] {
            ensure!(planned[id][k] == e[k], "planned excerpt differs");
        }
    }
    ensure!(
        ids(&q, "all_ids")? == expected.keys().cloned().collect()
            && ids(&q, "test_ids")? == expected.keys().cloned().collect(),
        "Test ID reservations differ"
    );
    for (key, purpose) in [
        ("test_query_ids", "query"),
        ("test_reference_ids", "reference"),
    ] {
        ensure!(
            ids(&q, key)?
                == expected
                    .iter()
                    .filter(|(_, r)| r["purpose"] == purpose)
                    .map(|(id, _)| id.clone())
                    .collect(),
            "query/reference allow-list differs"
        );
    }
    let hb = pinned(&plan["humans"], inputs)?;
    let rb = pinned(&plan["references"], inputs)?;
    ensure!(
        q["records_sha256"] == sha(&hb) && q["references_sha256"] == sha(&rb),
        "extractor output digests"
    );
    let humans = records(&hb)?;
    let references = records(&rb)?;
    ensure!(
        q["records"] == humans.len() && q["references"] == references.len(),
        "extractor output counts"
    );
    let mut seen = BTreeSet::new();
    for (rows, purpose, key) in [
        (&humans, "query", "emitted_human_ids"),
        (&references, "reference", "emitted_reference_ids"),
    ] {
        ensure!(
            ids(&q, key)? == unique_rows(rows)?.keys().cloned().collect(),
            "emitted ID list differs"
        );
        for r in rows {
            let id = str_at(r, "id")?;
            ensure!(
                seen.insert(id.to_owned()) && r["purpose"] == purpose,
                "excerpt duplicate/query-reference crossover"
            );
            let e = expected.get(id).context("unreserved excerpt")?;
            human(r, sources[&counter(e, "pg_id")?], e, &source_sha)?;
        }
    }
    let failures = array(&q, "failures")?.clone();
    let mut query_exclusions = Vec::new();
    let mut reference_exclusions = Vec::new();
    for f in &failures {
        let id = str_at(f, "id")?;
        let e = expected.get(id).context("unreserved extraction failure")?;
        ensure!(
            seen.insert(id.to_owned())
                && f["frozen_split"] == "test"
                && f["purpose"] == e["purpose"],
            "invalid/duplicate extraction failure"
        );
        str_at(f, "reason")?;
        let entry = json!({"id":id,"reason":"source_excerpt_extraction_failed","stage":"extraction","evidence":f});
        if e["purpose"] == "reference" {
            reference_exclusions.push(entry);
        } else {
            query_exclusions.push(entry);
            for model in MODELS {
                for percent in PERCENTAGES {
                    query_exclusions.push(json!({"id":balanced::task_id(id,model,percent),"parent_id":id,
                    "generator":model,"requested_span_percent":percent,"reason":"parent_excerpt_extraction_failed_no_generation_requested","stage":"extraction","evidence":f}));
                }
            }
        }
    }
    ensure!(
        seen == expected.keys().cloned().collect()
            && references.len() + reference_exclusions.len() == 4,
        "unaccounted excerpt"
    );
    Ok(Sources {
        works,
        humans,
        references,
        humans_sha: sha(&hb),
        source_sha,
        query_sha: sha(&qb),
        query_exclusions,
        reference_exclusions,
        extraction_failures: failures,
    })
}
