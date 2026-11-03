//! Render the frozen scientific-section candidates for manual admission review.
//! This tool emits no OriginRecords, split assignments, or model scores.
use anyhow::{Context, Result, bail, ensure};
use clap::Parser;
use roxmltree::{Document, Node, ParsingOptions};
use serde_json::{Value, json};
use slop_ninja_detector::dataset::sha256;
use std::{collections::BTreeMap, fs, io::Write, path::PathBuf};

#[derive(Parser)]
struct Args {
    #[arg(long)]
    inventory: PathBuf,
    #[arg(long)]
    expected_inventory_sha256: String,
    #[arg(long)]
    expected_candidates: usize,
    #[arg(long)]
    output: PathBuf,
    /// Preserve numeric/sign subscripts and superscripts as Unicode notation.
    #[arg(long)]
    numeric_scripts: bool,
}

#[derive(Clone, Copy, Default)]
enum RenderPolicy {
    #[default]
    ProseV1,
    NumericScriptsV2,
}

#[derive(Default)]
struct InlineAnnotations {
    citations: Vec<Value>,
    notation: Vec<Value>,
}

fn numeric_script(node: Node<'_, '_>) -> Result<(String, String)> {
    ensure!(
        node.children().all(|n| n.is_text()),
        "Nested markup in numeric script"
    );
    let source = node.children().filter_map(|n| n.text()).collect::<String>();
    ensure!(!source.is_empty(), "Empty numeric script");
    let digits: &[char] = if node.has_tag_name("sub") {
        &['₀', '₁', '₂', '₃', '₄', '₅', '₆', '₇', '₈', '₉', '₊', '₋']
    } else {
        &['⁰', '¹', '²', '³', '⁴', '⁵', '⁶', '⁷', '⁸', '⁹', '⁺', '⁻']
    };
    let rendered = source
        .chars()
        .map(|c| match c {
            '0'..='9' => Ok(digits[c as usize - '0' as usize]),
            '+' => Ok(digits[10]),
            '-' | '−' => Ok(digits[11]),
            _ => bail!("Unsupported character in numeric script: {c:?}"),
        })
        .collect::<Result<String>>()?;
    Ok((source, rendered))
}

fn string<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value[key]
        .as_str()
        .with_context(|| format!("Missing {key}"))
}

fn child<'a, 'b>(node: Node<'a, 'b>, name: &str) -> Result<Node<'a, 'b>> {
    let mut matching = node.children().filter(|n| n.has_tag_name(name));
    let value = matching.next().with_context(|| format!("Missing {name}"))?;
    ensure!(matching.next().is_none(), "Repeated {name}");
    Ok(value)
}

// Metadata only. Prose must go through the explicit renderer below.
fn metadata_text(node: Node<'_, '_>) -> String {
    node.descendants()
        .filter(Node::is_text)
        .filter_map(|n| n.text())
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn span(node: Node<'_, '_>, xml: &str) -> Value {
    json!({
        "raw_utf8_start":node.range().start,"raw_utf8_end":node.range().end,
        "raw_sha256":sha256(&xml.as_bytes()[node.range()])
    })
}

fn inline(
    node: Node<'_, '_>,
    xml: &str,
    ids: &BTreeMap<&str, Node<'_, '_>>,
    text: &mut String,
    annotations: &mut InlineAnnotations,
    in_citation: bool,
    policy: RenderPolicy,
) -> Result<()> {
    if node.is_text() {
        text.push_str(node.text().unwrap_or_default());
        return Ok(());
    }
    ensure!(node.is_element(), "Unsupported inline XML node");
    ensure!(
        node.tag_name().namespace().is_none(),
        "Namespaced inline element"
    );
    match node.tag_name().name() {
        "italic" | "bold" => {
            for n in node.children() {
                inline(n, xml, ids, text, annotations, in_citation, policy)?;
            }
        }
        "sub" | "sup" if matches!(policy, RenderPolicy::NumericScriptsV2) => {
            let (source, rendered) = numeric_script(node)?;
            text.push_str(&rendered);
            annotations.notation.push(json!({
                "span":span(node, xml),"kind":node.tag_name().name(),
                "source_text":source,"rendered_text":rendered
            }));
        }
        "xref" => {
            ensure!(!in_citation, "Nested citation");
            ensure!(
                node.attribute("ref-type") == Some("bibr"),
                "Non-bibliographic xref"
            );
            let rids: Vec<_> = node
                .attribute("rid")
                .unwrap_or_default()
                .split_whitespace()
                .collect();
            ensure!(!rids.is_empty(), "Citation has no target");
            let mut targets = Vec::new();
            for rid in rids {
                let target = *ids
                    .get(rid)
                    .with_context(|| format!("Missing citation target {rid}"))?;
                ensure!(
                    target.has_tag_name("ref"),
                    "Citation target is not a reference"
                );
                ensure!(
                    target.parent().is_some_and(|n| n.has_tag_name("ref-list"))
                        && target.ancestors().any(|n| n.has_tag_name("back")),
                    "Citation target is outside the article bibliography"
                );
                targets.push(json!({"id":rid,"span":span(target, xml),"reference_text":metadata_text(target)}));
            }
            let mut label = String::new();
            for n in node.children() {
                inline(n, xml, ids, &mut label, annotations, true, policy)?;
            }
            ensure!(!label.trim().is_empty(), "Empty visible citation label");
            text.push_str(&label);
            annotations
                .citations
                .push(json!({"span":span(node, xml),"visible_label":label,"targets":targets}));
        }
        name => bail!("Unsupported inline element: {name}"),
    }
    Ok(())
}

fn project(
    section: Node<'_, '_>,
    doc: &Document<'_>,
    xml: &str,
    policy: RenderPolicy,
) -> Result<Value> {
    let mut ids = BTreeMap::new();
    for n in doc.descendants().filter(Node::is_element) {
        if let Some(id) = n.attribute("id") {
            ensure!(ids.insert(id, n).is_none(), "Duplicate XML ID: {id}");
        }
    }
    let mut paragraphs = Vec::new();
    let mut texts = Vec::new();
    let mut inventory_texts = Vec::new();
    let mut heading = None;
    for n in section.children() {
        if n.is_text() {
            ensure!(
                n.text().unwrap_or_default().trim().is_empty(),
                "Text outside section paragraphs"
            );
            continue;
        }
        ensure!(
            n.is_element() && n.tag_name().namespace().is_none(),
            "Unsupported section XML node"
        );
        match n.tag_name().name() {
            "title" => {
                ensure!(
                    heading.is_none() && paragraphs.is_empty(),
                    "Repeated or misplaced section title"
                );
                heading = Some(json!({"text":metadata_text(n),"span":span(n, xml)}));
            }
            "p" => {
                let mut text = String::new();
                let mut annotations = InlineAnnotations::default();
                for c in n.children() {
                    inline(c, xml, &ids, &mut text, &mut annotations, false, policy)
                        .with_context(|| format!("Paragraph {}", paragraphs.len() + 1))?;
                }
                let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
                ensure!(!text.is_empty(), "Empty paragraph");
                let mut paragraph = json!({"index":paragraphs.len(),"span":span(n, xml),
                    "text_sha256":sha256(text.as_bytes()),"citations":annotations.citations});
                if matches!(policy, RenderPolicy::NumericScriptsV2) {
                    paragraph["notation"] = json!(annotations.notation);
                }
                paragraphs.push(paragraph);
                inventory_texts.push(metadata_text(n));
                texts.push(text);
            }
            name => bail!("Unsupported section element: {name}"),
        }
    }
    ensure!(
        heading.is_some() && !paragraphs.is_empty(),
        "Missing heading or prose"
    );
    let text = texts.join("\n\n");
    let mut projection = json!({"heading":heading,"paragraphs":paragraphs,
        "whitespace_words":text.split_whitespace().count(),"text_sha256":sha256(text.as_bytes()),"text":text,
        "changes":["decode XML entities","normalize intra-paragraph whitespace","join complete paragraphs with blank lines","keep heading in metadata","omit italic and bold typography while preserving their text"],
        "citation_policy":"visible labels and surrounding punctuation preserved; bibliography target bindings retained separately"});
    if matches!(policy, RenderPolicy::NumericScriptsV2) {
        projection["policy"] = json!("jats-prose-numeric-scripts-v2");
        projection["inventory_projection_sha256"] =
            json!(sha256(inventory_texts.join("\n\n").as_bytes()));
        projection["changes"].as_array_mut().unwrap().push(json!(
            "render digit/sign subscript and superscript as Unicode notation; retain each source span and conversion"
        ));
    }
    Ok(projection)
}

fn prepare(xml: &str, article: &Value, section: &Value, policy: RenderPolicy) -> Result<Value> {
    let doc = Document::parse_with_options(
        xml,
        ParsingOptions {
            allow_dtd: true,
            ..ParsingOptions::default()
        },
    )?;
    let root = doc.root_element();
    ensure!(root.has_tag_name("article"), "Not article XML");
    ensure!(
        root.attribute("article-type") == Some("research-article"),
        "Not research article"
    );
    ensure!(
        root.attribute(("http://www.w3.org/XML/1998/namespace", "lang")) == Some("en"),
        "Not English article"
    );
    let meta = child(child(root, "front")?, "article-meta")?;
    let dois: Vec<_> = meta
        .children()
        .filter(|n| n.has_tag_name("article-id") && n.attribute("pub-id-type") == Some("doi"))
        .map(metadata_text)
        .collect();
    ensure!(
        dois.len() == 1 && dois[0] == string(article, "doi")?,
        "DOI differs from inventory"
    );
    let section_index = section["body_section_index"]
        .as_u64()
        .context("Missing section index")? as usize;
    let node = child(root, "body")?
        .children()
        .filter(|n| n.has_tag_name("sec"))
        .nth(section_index)
        .context("Missing body section")?;
    ensure!(
        Some(node.range().start as u64) == section["raw_utf8_start"].as_u64()
            && Some(node.range().end as u64) == section["raw_utf8_end"].as_u64(),
        "Section range changed"
    );
    ensure!(
        sha256(&xml.as_bytes()[node.range()]) == string(section, "raw_section_sha256")?,
        "Section hash changed"
    );
    let publication = meta
        .children()
        .find(|n| n.has_tag_name("pub-date") && n.attribute("pub-type") == Some("epub"))
        .context("Missing publication date")?;
    let year = metadata_text(child(publication, "year")?).parse()?;
    let month = metadata_text(child(publication, "month")?).parse()?;
    let day = metadata_text(child(publication, "day")?).parse()?;
    let date =
        chrono::NaiveDate::from_ymd_opt(year, month, day).context("Invalid publication date")?;
    ensure!(year == 2019, "Outside fixed publication year");
    let authors = meta
        .children()
        .filter(|n| n.has_tag_name("contrib-group"))
        .flat_map(|n| n.children())
        .filter(|n| n.has_tag_name("contrib") && n.attribute("contrib-type") == Some("author"))
        .map(|n| {
            if let Ok(name) = child(n, "name") {
                let given = metadata_text(child(name, "given-names")?);
                let surname = metadata_text(child(name, "surname")?);
                ensure!(
                    !given.is_empty() && !surname.is_empty(),
                    "Empty author name"
                );
                Ok(format!("{given} {surname}"))
            } else {
                let name = metadata_text(child(n, "collab")?);
                ensure!(!name.is_empty(), "Empty credited collaboration");
                Ok(name)
            }
        })
        .collect::<Result<Vec<_>>>()?;
    ensure!(!authors.is_empty(), "No credited authors");
    let permissions: Vec<_> = meta
        .children()
        .filter(|n| n.has_tag_name("permissions"))
        .map(|n| json!({"span":span(n, xml),"text":metadata_text(n),"xml":&xml[n.range()]}))
        .collect();
    ensure!(!permissions.is_empty(), "Missing article permission notice");
    let related: Vec<_> = meta
        .children()
        .filter(|n| n.has_tag_name("related-article") || n.has_tag_name("related-object"))
        .map(|n| json!({"span":span(n, xml),"xml":&xml[n.range()]}))
        .collect();
    let projection = project(node, &doc, xml, policy)?;
    ensure!(
        projection["paragraphs"].as_array().map(|p| p.len() as u64)
            == section["paragraphs"].as_u64(),
        "Paragraph count differs from inventory"
    );
    ensure!(
        projection
            .get("inventory_projection_sha256")
            .unwrap_or(&projection["text_sha256"])
            == &section["paragraph_projection_sha256"],
        "Supported projection differs from inventory"
    );
    Ok(
        json!({"source_group":article["source_group"],"doi":dois[0],"title":article["title"],
        "publication_date":date.to_string(),"credited_authors":authors,"permissions":permissions,"related_items":related,
        "section_span":span(node, xml),"projection":projection,
        "rights_status":"pending_item_review","intended_evidence":"historical_proxy",
        "historical_byte_identity_verified":false,"documented_human_workflow":false,
        "manual_rendering_review_complete":false,"duplicate_review_complete":false,"admitted":false}),
    )
}

fn main() -> Result<()> {
    let args = Args::parse();
    let policy = if args.numeric_scripts {
        RenderPolicy::NumericScriptsV2
    } else {
        RenderPolicy::ProseV1
    };
    ensure!(!args.output.exists(), "Use a new preparation output");
    let bytes = fs::read(&args.inventory)?;
    ensure!(
        sha256(&bytes) == args.expected_inventory_sha256,
        "Inventory hash mismatch"
    );
    let inventory: Value = serde_json::from_slice(&bytes)?;
    ensure!(
        inventory["schema"] == "slop_ninja.plos_section_inventory.v1",
        "Unknown inventory schema"
    );
    for binding in inventory["source_files"]
        .as_array()
        .context("Missing exclusion bindings")?
    {
        ensure!(
            sha256(fs::read(string(binding, "path")?)?) == string(binding, "sha256")?,
            "Changed exclusion input"
        );
    }
    let mut candidates = Vec::new();
    for row in inventory["rows"]
        .as_array()
        .context("Missing inventory rows")?
    {
        let article = &row["article"];
        if article["already_in_exclusion_inputs"] != false
            || article["english"] != true
            || article["research_article"] != true
            || article["publication_year"] != "2019"
            || article["has_explicit_cc_by_link"] != true
        {
            continue;
        }
        for section in article["sections"]
            .as_array()
            .context("Missing inventoried sections")?
        {
            if section["within_review_word_range"] == true {
                candidates.push((row, article, section));
            }
        }
    }
    ensure!(
        args.expected_candidates > 0 && candidates.len() == args.expected_candidates,
        "Candidate pool count mismatch"
    );
    candidates
        .sort_by_key(|(_, _, section)| section["raw_section_sha256"].as_str().unwrap_or_default());
    let mut rows = Vec::new();
    for (row, article, section) in candidates {
        let raw = fs::read(string(row, "raw_path")?)?;
        let capture = fs::read(string(row, "metadata_path")?)?;
        ensure!(
            sha256(&raw) == string(row, "raw_sha256")?,
            "Changed article capture"
        );
        ensure!(
            sha256(&capture) == string(row, "metadata_sha256")?,
            "Changed capture metadata"
        );
        let capture: Value = serde_json::from_slice(&capture)?;
        ensure!(
            capture["body_sha256"] == row["raw_sha256"]
                && capture["request_url"] == row["request_url"],
            "Capture binding mismatch"
        );
        let result = std::str::from_utf8(&raw)
            .context("Non-UTF8 article")
            .and_then(|xml| prepare(xml, article, section, policy));
        rows.push(json!({"source_group":article["source_group"],"doi":article["doi"],
            "raw_path":row["raw_path"],"raw_sha256":row["raw_sha256"],
            "section_sha256":section["raw_section_sha256"],
            "status":if result.is_ok(){"prepared_for_manual_review"}else{"unsupported_or_invalid"},
            "prepared":result.as_ref().ok(),"preparation_error":result.err().map(|e|format!("{e:#}"))}));
    }
    let supported = rows.iter().filter(|r| r["prepared"].is_object()).count();
    let report = json!({"schema":"slop_ninja.plos_section_preparation.v1", "inventory_sha256":args.expected_inventory_sha256,
        "candidate_count":rows.len(),"prepared_count":supported,"unsupported_or_invalid_count":rows.len()-supported,
        "admitted_records":0,"split_assigned":false,"network_calls":0,"model_calls":0,"rows":rows,
        "limitations":["Preparation is not item admission, grammatical certification or proof of human authorship.",
            "All candidates remain pending final manual rendering, rights, related-work, duplicate and tokenizer review.",
            "Unsupported candidates are retained whole; no passages are silently removed or corrected."]});
    let mut out = fs::File::create_new(args.output)?;
    serde_json::to_writer_pretty(&mut out, &report)?;
    out.write_all(b"\n")?;
    println!(
        "{}",
        json!({"candidate_count":report["candidate_count"],"prepared_count":supported,
        "unsupported_or_invalid_count":report["unsupported_or_invalid_count"],"admitted_records":0})
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(content: &str, bibliography: &str) -> String {
        format!(
            "<article><body><sec><title>Introduction</title>{content}</sec></body><back><ref-list>{bibliography}</ref-list></back></article>"
        )
    }

    fn render(xml: &str) -> Result<Value> {
        let doc = Document::parse(xml)?;
        let section = doc.descendants().find(|n| n.has_tag_name("sec")).unwrap();
        project(section, &doc, xml, RenderPolicy::ProseV1)
    }

    #[test]
    fn preserves_paragraphs_citation_ranges_and_reference_bindings() {
        let xml = fixture(
            "<p>First <italic>claim</italic> [<xref ref-type=\"bibr\" rid=\"r1\">1</xref>–<xref ref-type=\"bibr\" rid=\"r2\">2</xref>] &amp; caveat.</p><p>Opposed claim.</p>",
            "<ref id=\"r1\">One reference.</ref><ref id=\"r2\">Another reference.</ref>",
        );
        let result = render(&xml).unwrap();
        assert_eq!(
            result["text"],
            "First claim [1–2] & caveat.\n\nOpposed claim."
        );
        let paragraphs = result["paragraphs"].as_array().unwrap();
        assert_eq!(paragraphs.len(), 2);
        assert_eq!(paragraphs[0]["citations"][1]["targets"][0]["id"], "r2");
        assert_eq!(
            paragraphs[0]["citations"][1]["targets"][0]["reference_text"],
            "Another reference."
        );
        let start = paragraphs[1]["span"]["raw_utf8_start"].as_u64().unwrap() as usize;
        let end = paragraphs[1]["span"]["raw_utf8_end"].as_u64().unwrap() as usize;
        assert_eq!(&xml[start..end], "<p>Opposed claim.</p>");
    }

    #[test]
    fn rejects_lost_content_and_invalid_reference_resolution() {
        for content in [
            "<p>Plain <sub>2A</sub> receptor.</p>",
            "<p>Plain.</p><sec><title>More</title><p>Required.</p></sec>",
            "<p>Plain.</p><fig><caption><p>Required.</p></caption></fig>",
            "<p>Claim <xref ref-type=\"bibr\" rid=\"missing\">1</xref>.</p>",
            "<p>Claim <xref ref-type=\"fig\" rid=\"r1\">1</xref>.</p>",
        ] {
            assert!(
                render(&fixture(content, "<ref id=\"r1\">A reference.</ref>")).is_err(),
                "{content}"
            );
        }
        assert!(
            render(&fixture(
                "<p>Claim <xref ref-type=\"bibr\" rid=\"r1\">1</xref>.</p>",
                "<ref id=\"r1\">One.</ref><ref id=\"r1\">Two.</ref>"
            ))
            .is_err()
        );
    }

    #[test]
    fn numeric_notation_is_explicit_traceable_and_never_flattened() {
        let xml = fixture(
            "<p>N<sup>6</sup>, CO<sub>2</sub>, cm<sup>−3</sup>, Ca<sup>++</sup> [<xref ref-type=\"bibr\" rid=\"r1\">1</xref>].</p>",
            "<ref id=\"r1\">One reference.</ref>",
        );
        assert!(render(&xml).is_err());
        let doc = Document::parse(&xml).unwrap();
        let section = doc.descendants().find(|n| n.has_tag_name("sec")).unwrap();
        let result = project(section, &doc, &xml, RenderPolicy::NumericScriptsV2).unwrap();
        assert_eq!(result["text"], "N⁶, CO₂, cm⁻³, Ca⁺⁺ [1].");
        assert_eq!(
            result["inventory_projection_sha256"],
            sha256("N6, CO2, cm−3, Ca++ [1].".as_bytes())
        );
        let notation = result["paragraphs"][0]["notation"].as_array().unwrap();
        assert_eq!(notation.len(), 4);
        for (entry, original) in notation.iter().zip([
            "<sup>6</sup>",
            "<sub>2</sub>",
            "<sup>−3</sup>",
            "<sup>++</sup>",
        ]) {
            let start = entry["span"]["raw_utf8_start"].as_u64().unwrap() as usize;
            let end = entry["span"]["raw_utf8_end"].as_u64().unwrap() as usize;
            assert_eq!(&xml[start..end], original);
            assert_eq!(entry["span"]["raw_sha256"], sha256(original.as_bytes()));
        }
        assert_eq!(
            result["paragraphs"][0]["citations"][0]["targets"][0]["id"],
            "r1"
        );
        for script in [
            "<sub>1A</sub>",
            "<sup>th</sup>",
            "<sup>1 2</sup>",
            "<sup><italic>2</italic></sup>",
            "<sup/>",
        ] {
            let xml = fixture(&format!("<p>X{script}.</p>"), "");
            let doc = Document::parse(&xml).unwrap();
            let section = doc.descendants().find(|n| n.has_tag_name("sec")).unwrap();
            assert!(project(section, &doc, &xml, RenderPolicy::NumericScriptsV2).is_err());
        }
    }
}
