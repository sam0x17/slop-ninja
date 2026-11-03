//! Build review-only ordered document views from the frozen eight-work packet.
use anyhow::{Context, Result, ensure};
use clap::Parser;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, fs, io::Write, path::PathBuf};
use unslop::{global_voices, html_document};

fn digest(bytes: impl AsRef<[u8]>) -> String {
    hex::encode(Sha256::digest(bytes))
}

#[derive(Parser)]
struct Args {
    #[arg(long)]
    cache: PathBuf,
    #[arg(long)]
    packet: PathBuf,
    #[arg(long)]
    out: PathBuf,
}

fn string(v: &Value) -> Result<&str> {
    v.as_str().context("Expected string")
}

fn main() -> Result<()> {
    let args = Args::parse();
    ensure!(!args.out.exists(), "Use a fresh output directory");
    let packet_bytes = fs::read(&args.packet)?;
    ensure!(
        digest(&packet_bytes) == "572c9f9cc5f5e3efd755fc688bb7308339f9914b73c59f937f79c70220806b16",
        "Changed review packet"
    );
    let packet: Value = serde_json::from_slice(&packet_bytes)?;
    let selected = packet["selected"].as_array().context("Missing selection")?;
    ensure!(selected.len() == 8, "Expected eight selected works");
    fs::create_dir(&args.out)?;
    fs::create_dir(args.out.join("works"))?;
    let mut projection = fs::File::create_new(args.out.join("review.jsonl"))?;
    let mut summaries = Vec::new();
    for chosen in selected {
        let record = &chosen["record"];
        let id = string(&record["id"])?;
        let provenance = &record["provenance"];
        let key = digest(string(&provenance["api_url"])?);
        let body = fs::read(args.cache.join("raw").join(format!("{key}.body")))?;
        ensure!(
            digest(&body) == provenance["api_sha256"],
            "Changed API body"
        );
        let posts: Vec<Value> = serde_json::from_slice(&body)?;
        let matching: Vec<_> = posts
            .iter()
            .filter(|p| p["link"] == provenance["source_url"])
            .collect();
        ensure!(matching.len() == 1, "Nonunique article URL");
        let html = string(&matching[0]["content"]["rendered"])?;
        ensure!(
            digest(html) == provenance["content_html_sha256"],
            "Changed article HTML"
        );
        let legacy = global_voices::extract_content(html);
        ensure!(
            legacy.text == record["text"] && digest(&legacy.text) == record["text_sha256"],
            "Old text extraction changed"
        );
        ensure!(
            serde_json::to_value(&legacy.paragraphs)? == record["paragraphs"],
            "Old paragraph decisions changed"
        );
        let document = html_document::observe(html, html_document::Limits::default())?;
        let paragraphs: Vec<_> = document
            .nodes
            .iter()
            .filter(|n| n.paragraph_ordinal.is_some())
            .collect();
        ensure!(
            paragraphs.len() == legacy.paragraphs.len(),
            "Paragraph coverage differs; inspect DOM ordering"
        );
        for p in &paragraphs {
            let mut text = String::new();
            for node in &document.nodes[p.id..p.subtree_end_exclusive] {
                match &node.value {
                    html_document::Value::Text { decoded } => text.push_str(decoded),
                    html_document::Value::Element { name, .. }
                        if ["br", "p", "div", "blockquote"].contains(&name.as_str()) =>
                    {
                        text.push(' ')
                    }
                    _ => {}
                }
            }
            ensure!(
                text.split_whitespace().collect::<Vec<_>>().join(" ")
                    == legacy.paragraphs[p.paragraph_ordinal.unwrap()].text,
                "Recovered paragraph does not match legacy crosswalk"
            );
        }
        let mut owned_text = vec![Vec::new(); document.nodes.len()];
        let mut child_regions = vec![Vec::new(); document.nodes.len()];
        for node in &document.nodes {
            if let Some(parent) = node.region_parent {
                if !node.roles.is_empty() {
                    child_regions[parent].push(node.id);
                }
                if let html_document::Value::Text { decoded } = &node.value {
                    owned_text[parent].push(json!({"node_id":node.id,"decoded":decoded}));
                }
            }
        }
        let mut roles = BTreeMap::<String, usize>::new();
        for &region_id in &document.region_ids {
            let node = &document.nodes[region_id];
            for role in &node.roles {
                *roles.entry(role.clone()).or_default() += 1;
            }
            // Each decoded text node appears under its nearest region once.
            // Descendant regions retain their own text, in the full DOM order.
            let old = node.paragraph_ordinal.map(|n| &legacy.paragraphs[n]);
            let row = json!({"work_id":id,"node_id":node.id,"parent_region":node.region_parent,"subtree_end_exclusive":node.subtree_end_exclusive,
                "roles":node.roles,"element":node.value,"paragraph_ordinal":node.paragraph_ordinal,
                "legacy_excluded":old.and_then(|p|p.excluded.as_deref()),"legacy_paragraph_text":old.map(|p|p.text.as_str()),
                "direct_child_regions":child_regions[region_id],"owned_text_nodes":owned_text[region_id],"author_or_speaker_assigned":false});
            serde_json::to_writer(&mut projection, &row)?;
            writeln!(projection)?;
        }
        let out_name = format!("{}.json", id.replace(':', "-"));
        ensure!(
            id.strip_prefix("gv:")
                .is_some_and(|x| !x.is_empty() && x.bytes().all(|b| b.is_ascii_digit())),
            "Unexpected work ID"
        );
        let artifact = json!({"source_record":record,"document":document,"legacy_extraction":legacy,
            "training_export":false,"origin_label_assigned":false});
        let bytes = serde_json::to_vec_pretty(&artifact)?;
        fs::write(args.out.join("works").join(&out_name), &bytes)?;
        summaries.push(json!({"id":id,"source_text_sha256":record["text_sha256"],"html_sha256":provenance["content_html_sha256"],
            "artifact":format!("works/{out_name}"),"artifact_sha256":digest(&bytes),"nodes":document.nodes.len(),
            "regions":document.region_ids.len(),"roles":roles,"paragraphs":paragraphs.len(),
            "old_retained_paragraphs":legacy.paragraphs.iter().filter(|p|p.excluded.is_none()).count(),
            "old_excluded_paragraphs":legacy.paragraphs.iter().filter(|p|p.excluded.is_some()).count(),
            "parse_errors":document.parse_errors,"unassigned_non_whitespace_text_nodes":document.unassigned_non_whitespace_text_ids.len(),
            "old_extraction_unchanged":true}));
    }
    projection.sync_all()?;
    let report = json!({"schema":"slop_ninja_global_voices_document_view_run_v1","at":chrono::Utc::now().to_rfc3339(),
        "packet_sha256":digest(&packet_bytes),"view_version":html_document::VERSION,
        "view_source_sha256":digest(include_bytes!("../src/html_document.rs")),"runner_source_sha256":digest(include_bytes!("global_voices_document_view.rs")),
        "legacy_extractor_sha256":digest(include_bytes!("../src/global_voices.rs")),"util_source_sha256":digest(include_bytes!("../src/util.rs")),
        "lib_source_sha256":digest(include_bytes!("../src/lib.rs")),"binary_sha256":digest(fs::read(std::env::current_exe()?)?),
        "protocol_sha256":digest(include_bytes!("../baseline/detector/GLOBAL_VOICES_DOCUMENT_VIEW_V1.md")),
        "cargo_lock_sha256":digest(include_bytes!("../Cargo.lock")),"works":summaries,
        "review_projection_sha256":digest(fs::read(args.out.join("review.jsonl"))?),"manual_review_complete":false,
        "training_export":false,"new_source_requests":0,"model_calls":0,"pangram_calls":0});
    let bytes = serde_json::to_vec_pretty(&report)?;
    fs::write(args.out.join("report.json"), &bytes)?;
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}
