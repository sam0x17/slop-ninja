//! Materialize reviewed, unsplit historical proxies from complete JATS sections.
use anyhow::{Context, Result, ensure};
use clap::Parser;
use serde_json::{Value, json};
use slop_ninja_detector::dataset::{self, Evidence, Origin, OriginRecord, Rights, Source, sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Write,
    path::{Path, PathBuf},
};

#[derive(Parser)]
struct Args {
    #[arg(long)]
    declaration: PathBuf,
    #[arg(long)]
    output: PathBuf,
}

fn text<'a>(v: &'a Value, key: &str) -> Result<&'a str> {
    v[key].as_str().with_context(|| format!("Missing {key}"))
}

fn load(base: &Path, binding: &Value) -> Result<Value> {
    let bytes = fs::read(base.join(text(binding, "path")?))?;
    ensure!(
        binding["sha256"] == sha256(&bytes),
        "Changed admission input"
    );
    Ok(serde_json::from_slice(&bytes)?)
}

fn main() -> Result<()> {
    let a = Args::parse();
    ensure!(!a.output.exists(), "Use a new admission output directory");
    let base = a
        .declaration
        .parent()
        .context("Missing declaration parent")?;
    let declaration_bytes = fs::read(&a.declaration)?;
    let declaration: Value = serde_json::from_slice(&declaration_bytes)?;
    ensure!(
        declaration["schema"] == "slop_ninja_plos_admission_declaration_v1",
        "Unknown declaration"
    );
    let prep = load(base, &declaration["preparation"])?;
    let review = load(base, &declaration["manual_review"])?;
    let tokens = load(base, &declaration["tokenizer_audit"])?;
    let overlap = load(base, &declaration["overlap_report"])?;
    let overlap_plan = load(base, &declaration["overlap_plan"])?;
    let preparation_hash = &declaration["preparation"]["sha256"];
    ensure!(
        review["preparation_sha256"] == *preparation_hash
            && tokens["preparation_sha256"] == *preparation_hash
            && overlap_plan["preparation"]["sha256"] == *preparation_hash
            && overlap["plan_sha256"] == declaration["overlap_plan"]["sha256"],
        "Inconsistent preparation bindings"
    );
    ensure!(
        tokens["truncation"] == false
            && tokens["include_special_tokens"] == true
            && tokens["tokenizer_files_verified_against_pin"] == true,
        "Unsupported tokenizer audit"
    );
    let decisions: BTreeMap<_, _> = declaration["decisions"]
        .as_array()
        .context("Missing decisions")?
        .iter()
        .map(|r| Ok((text(r, "doi")?, r)))
        .collect::<Result<_>>()?;
    let rows = prep["rows"]
        .as_array()
        .context("Missing preparation rows")?;
    ensure!(
        decisions.len() == rows.len()
            && decisions.len() == declaration["decisions"].as_array().unwrap().len(),
        "Decision coverage mismatch"
    );
    let mut records = Vec::new();
    let mut dispositions = Vec::new();
    let mut aliases = BTreeMap::new();
    for row in rows {
        let doi = text(row, "doi")?;
        let decision = decisions.get(doi).context("Candidate lacks decision")?;
        ensure!(
            matches!(decision["decision"].as_str(), Some("admit" | "pending")),
            "Unknown disposition"
        );
        ensure!(
            !text(decision, "reason")?.trim().is_empty(),
            "Missing review reason"
        );
        if decision["decision"] != "admit" {
            dispositions.push(json!({"doi":doi,"decision":"pending","reason":decision["reason"],"section_sha256":row["section_sha256"]}));
            continue;
        }
        let p = &row["prepared"];
        ensure!(p.is_object(), "Cannot admit an unsupported rendering");
        let projection = &p["projection"];
        let prose = text(projection, "text")?;
        let prose_hash = sha256(prose.as_bytes());
        ensure!(projection["text_sha256"] == prose_hash, "Changed rendering");
        let item_review = review["manual_rendering_reviews"]
            .as_array()
            .context("Missing manual reviews")?
            .iter()
            .find(|r| r["doi"] == doi)
            .context("Missing complete rendering review")?;
        ensure!(
            item_review["complete_rendering_read"] == true
                && item_review["text_sha256"] == prose_hash
                && item_review["attributed_authors_checked"] == p["credited_authors"],
            "Rendering or attribution lacks matching review"
        );
        ensure!(
            !review["manual_review_followups"]["quotation_rights_pending_dois"]
                .as_array()
                .context("Missing quotation dispositions")?
                .iter()
                .any(|v| v == doi),
            "Quotation permission remains pending"
        );
        let token_row = tokens["rows"]
            .as_array()
            .context("Missing token counts")?
            .iter()
            .find(|r| r["doi"] == doi)
            .context("No tokenizer row")?;
        ensure!(
            token_row["text_sha256"] == prose_hash
                && token_row["whole_document_supported"] == true
                && token_row["a_tokens"]
                    .as_u64()
                    .is_some_and(|n| n > 0 && n <= 2048),
            "Whole text exceeds supported token scope"
        );
        let duplicate_row = overlap["candidate_rows"]
            .as_array()
            .context("Missing overlap rows")?
            .iter()
            .find(|r| r["doi"] == doi)
            .context("No overlap row")?;
        ensure!(
            duplicate_row["text_sha256"] == prose_hash
                && duplicate_row["existing_group_match"] == false
                && duplicate_row["flagged_matches"]
                    .as_array()
                    .is_some_and(Vec::is_empty),
            "Unresolved overlap"
        );
        let raw = fs::read(text(row, "raw_path")?)?;
        ensure!(row["raw_sha256"] == sha256(&raw), "Changed captured XML");
        let permissions = p["permissions"]
            .as_array()
            .context("Missing permission notice")?;
        ensure!(
            permissions.len() == 1,
            "Requires a separately reviewed permission mapping"
        );
        let permission = &permissions[0];
        let start = permission["span"]["raw_utf8_start"]
            .as_u64()
            .context("Missing notice start")? as usize;
        let end = permission["span"]["raw_utf8_end"]
            .as_u64()
            .context("Missing notice end")? as usize;
        let notice = raw.get(start..end).context("Invalid notice span")?;
        ensure!(
            sha256(notice) == permission["span"]["raw_sha256"]
                && std::str::from_utf8(notice)? == text(permission, "xml")?,
            "Notice differs from reviewed capture"
        );
        let notice_xml = format!(
            "<root xmlns:xlink=\"http://www.w3.org/1999/xlink\">{}</root>",
            text(permission, "xml")?
        );
        let notice_doc = roxmltree::Document::parse(&notice_xml)?;
        let licenses: Vec<_> = notice_doc
            .descendants()
            .filter(|n| n.has_tag_name("license"))
            .filter_map(|n| n.attribute(("http://www.w3.org/1999/xlink", "href")))
            .collect();
        ensure!(
            licenses.len() == 1
                && matches!(
                    licenses[0],
                    "http://creativecommons.org/licenses/by/4.0/"
                        | "https://creativecommons.org/licenses/by/4.0/"
                ),
            "Unsupported license mapping"
        );
        let own_group = text(p, "source_group")?;
        let mut family = BTreeSet::from([own_group.to_owned()]);
        for relation in p["related_items"]
            .as_array()
            .context("Missing related items")?
        {
            let wrapped = format!(
                "<root xmlns:xlink=\"http://www.w3.org/1999/xlink\">{}</root>",
                text(relation, "xml")?
            );
            let doc = roxmltree::Document::parse(&wrapped)?;
            let node = doc
                .descendants()
                .find(|n| n.has_tag_name("related-article"))
                .context("Missing related article")?;
            ensure!(
                node.attribute("related-article-type") == Some("companion"),
                "Unreviewed relation type"
            );
            let href = node
                .attribute(("http://www.w3.org/1999/xlink", "href"))
                .context("No companion link")?;
            let companion = href
                .strip_prefix("info:doi/")
                .context("Unreviewed related link scheme")?;
            let group = format!("doi:{}", companion.to_lowercase());
            ensure!(
                !overlap["observed_groups"]
                    .as_array()
                    .context("Missing observed groups")?
                    .iter()
                    .any(|v| v == &group),
                "Companion already present: resolve grouping first"
            );
            family.insert(group);
        }
        let declared_aliases: BTreeSet<_> = decision["family_aliases"]
            .as_array()
            .context("Missing reviewed family aliases")?
            .iter()
            .map(|v| {
                v.as_str()
                    .map(str::to_owned)
                    .context("Invalid family alias")
            })
            .collect::<Result<_>>()?;
        ensure!(
            family == declared_aliases,
            "Related family differs from reviewed decision"
        );
        let group = family.first().unwrap().clone();
        for alias in &family {
            ensure!(
                aliases.insert(alias.clone(), group.clone()).is_none(),
                "Repeated source-family alias"
            );
        }
        let authors: Vec<String> = serde_json::from_value(p["credited_authors"].clone())?;
        ensure!(!authors.is_empty(), "Missing credited authors");
        let date = text(p, "publication_date")?;
        let title = text(p, "title")?;
        let changes: Vec<String> = serde_json::from_value(projection["changes"].clone())?;
        let attribution = format!(
            "{} ({date}). {title}. PLOS ONE. https://doi.org/{doi}. CC BY 4.0 (https://creativecommons.org/licenses/by/4.0/). Complete Introduction section; extraction changes: {}. Credited authors are not verified individual prose authors.",
            authors.join("; "),
            changes.join("; ")
        );
        let record = OriginRecord {
            schema:dataset::RECORD_SCHEMA.to_owned(), id:format!("plos:{doi}:introduction:{}",&prose_hash[..16]),
            source_group:group, split:None, origin:Origin::HumanOnly, evidence:Evidence::HistoricalProxy,
            evidence_notes:"Weak historical-proxy label based on the publisher's 2019 publication metadata. Captured bytes have no verified immutable historical revision or documented human-only workflow. Full rendering, credited authors and permission notice were inspected. Lexical screening does not establish source independence or scientific correctness.".to_owned(),
            text:prose.to_owned(),text_sha256:prose_hash,
            source:Source {collection:"plos-introductions-historical-v1".to_owned(),url:format!("https://doi.org/{doi}"),
                version:format!("captured-jats:{}",text(row,"raw_sha256")?),published_at:Some(date.to_owned()),
                author_ids:authors,raw_path:text(row,"raw_path")?.to_owned(),raw_sha256:text(row,"raw_sha256")?.to_owned(),
                extraction:format!("Complete Introduction; {}. Citation labels retained; bibliography target bindings in preparation {}.",changes.join("; "),preparation_hash.as_str().unwrap())},
            rights:Rights {license:"CC-BY-4.0".to_owned(),evidence_url:format!("https://journals.plos.org/plosone/article?id={doi}"),
                evidence_sha256:text(&permission["span"],"raw_sha256")?.to_owned(),attribution,
                commercial_training:true,model_release:true,external_evaluation:true,redistribute_text:true,share_alike:None},
            parent_id:None,generation:None,
        };
        record.validate()?;
        dispositions.push(json!({"doi":doi,"decision":"admitted_unsplit_historical_proxy","reason":decision["reason"],
            "id":record.id,"source_group":record.source_group,"family_aliases":family,"text_sha256":record.text_sha256,
            "a_tokens":token_row["a_tokens"],"attribution":record.rights.attribution}));
        records.push(record);
    }
    dataset::validate_records(&records)?;
    fs::create_dir(&a.output)?;
    dataset::write_records(&a.output.join("human.jsonl"), &records)?;
    let report = json!({"schema":"slop_ninja_plos_section_admission_v1","declaration_sha256":sha256(declaration_bytes),
        "records_sha256":sha256(fs::read(a.output.join("human.jsonl"))?),"candidates":rows.len(),
        "admitted_records":records.len(),"pending_records":rows.len()-records.len(),"family_aliases":aliases,
        "decisions":dispositions,"evidence":"historical_proxy","split_assigned":false,"model_calls":0,
        "active_fits_changed":false,"commercial_training_corpus_preparation_only":true});
    let mut file = fs::File::create_new(a.output.join("report.json"))?;
    serde_json::to_writer_pretty(&mut file, &report)?;
    file.write_all(b"\n")?;
    println!(
        "{}",
        json!({"candidates":rows.len(),"admitted":records.len(),"pending":rows.len()-records.len(),"split_assigned":false})
    );
    Ok(())
}
